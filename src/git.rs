//! Typed, no-shell Git operations used by repository updates.
//!
//! Callers choose an operation, never an arbitrary subcommand or flag. This is
//! the enforcement boundary for the update feature: inspection operations run
//! with Git's optional writes disabled, fetch is explicitly non-interactive,
//! and the sole worktree write (`merge --ff-only`) deliberately receives none
//! of the inspection overrides.

use std::{
    collections::HashSet,
    env,
    ffi::OsString,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{Error, Result};

const UPDATE_SUBCOMMANDS: &[&str] = &[
    "fetch",
    "rev-parse",
    "symbolic-ref",
    "for-each-ref",
    "config",
    "merge-base",
    "rev-list",
    "diff-tree",
    "ls-tree",
    "status",
    "ls-files",
    "cat-file",
    "check-attr",
    "update-ref",
    "merge",
    // `--get-url` only expands configuration; it is documented to exit without
    // contacting the remote.
    "ls-remote",
];

const MAX_FILTERED_STATUS_PATHS: usize = 4_096;
const MAX_FILTERED_STATUS_PATH_BYTES: usize = 1024 * 1024;
const MAX_FETCH_OUTPUT_BYTES: usize = 64 * 1024;
const REPOSITORY_ROUTING_ENVIRONMENT: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_CONFIG_COUNT",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HeadState {
    Branch { reference: String, revision: String },
    Detached { revision: String },
}

impl HeadState {
    pub fn revision(&self) -> &str {
        match self {
            Self::Branch { revision, .. } | Self::Detached { revision } => revision,
        }
    }

    pub fn reference(&self) -> Option<&str> {
        match self {
            Self::Branch { reference, .. } => Some(reference),
            Self::Detached { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Upstream {
    branch: String,
    remote: String,
    merge_ref: String,
    tracking_ref: String,
    /// The object the tracking ref names, when it names one.
    ///
    /// `None` is a configured upstream whose remote-tracking ref is not there:
    /// pruned, deleted, or never fetched. That is not "no upstream" — the
    /// remote, the merge ref, and the fetch mapping are all still configured,
    /// and the fetch an explicit check performs is precisely what puts the ref
    /// back. Treating the absent revision as an unconfigured upstream would
    /// skip that fetch and leave the repository unable to check or apply an
    /// update until the user fetched by hand.
    revision: Option<String>,
}

impl Upstream {
    pub fn branch(&self) -> &str {
        &self.branch
    }
    pub fn remote(&self) -> &str {
        &self.remote
    }
    pub fn merge_ref(&self) -> &str {
        &self.merge_ref
    }
    pub fn tracking_ref(&self) -> &str {
        &self.tracking_ref
    }
    pub fn revision(&self) -> Option<&str> {
        self.revision.as_deref()
    }

    pub(crate) fn with_revision(&self, revision: String) -> Self {
        let mut fetched = self.clone();
        fetched.revision = Some(revision);
        fetched
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AheadBehind {
    pub ahead: usize,
    pub behind: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    TypeChanged,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChangedPath {
    kind: ChangeKind,
    path: PathBuf,
    gitlink: bool,
    renamed_from: Option<PathBuf>,
}

impl ChangedPath {
    pub fn kind(&self) -> ChangeKind {
        self.kind
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn is_gitlink(&self) -> bool {
        self.gitlink
    }
    pub fn renamed_from(&self) -> Option<&Path> {
        self.renamed_from.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeState {
    pub index_dirty: bool,
    pub worktree_dirty: bool,
    /// False when configured clean/process filters make the safe, filter-free
    /// status result ambiguous.
    pub worktree_dirty_known: bool,
    pub untracked: Vec<PathBuf>,
    /// Ignored paths are not dirtiness by themselves. They are retained only
    /// so an incoming tracked path cannot silently overwrite local content.
    pub ignored: Vec<PathBuf>,
}

impl Default for WorktreeState {
    fn default() -> Self {
        Self {
            index_dirty: false,
            worktree_dirty: false,
            worktree_dirty_known: true,
            untracked: Vec::new(),
            ignored: Vec::new(),
        }
    }
}

impl WorktreeState {
    pub fn tracked_dirty(&self) -> bool {
        self.index_dirty || self.worktree_dirty
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationFixture {
    pub subcommand: &'static str,
    pub arguments: Vec<OsString>,
    pub environment: Vec<(OsString, OsString)>,
}

#[derive(Clone, Debug)]
enum UpdateOp {
    RevParse(String),
    AbsoluteGitDir,
    SymbolicHead,
    SymbolicRef(String),
    UpstreamRef(String),
    RefState(String),
    ConfigGet(String),
    TransportSettings,
    TransportPolicy,
    UserSshCommand,
    RemoteUrl(String),
    FilterSettings,
    PromisorSettings,
    CheckAttr,
    MergeBase(String, String),
    AheadBehind(String, String),
    ChangedPaths(String, String),
    CommitSummaries(String, String),
    CatFile(String),
    TreeEntryMode {
        revision: String,
        path: PathBuf,
    },
    Status,
    Fetch {
        remote: String,
        refspec: String,
        ssh_command: String,
        allowed_protocols: String,
        hooks_path: PathBuf,
    },
    PublishRef {
        reference: String,
        revision: String,
        expected: String,
        hooks_path: PathBuf,
    },
    Merge(String),
}

impl UpdateOp {
    fn subcommand(&self) -> &'static str {
        match self {
            Self::RevParse(_) | Self::AbsoluteGitDir => "rev-parse",
            Self::SymbolicHead | Self::SymbolicRef(_) => "symbolic-ref",
            Self::UpstreamRef(_) | Self::RefState(_) => "for-each-ref",
            Self::ConfigGet(_)
            | Self::FilterSettings
            | Self::PromisorSettings
            | Self::TransportSettings
            | Self::TransportPolicy
            | Self::UserSshCommand => "config",
            Self::CheckAttr => "check-attr",
            Self::MergeBase(_, _) => "merge-base",
            Self::AheadBehind(_, _) | Self::CommitSummaries(_, _) => "rev-list",
            Self::ChangedPaths(_, _) => "diff-tree",
            Self::CatFile(_) => "cat-file",
            Self::TreeEntryMode { .. } => "ls-tree",
            Self::Status => "status",
            Self::Fetch { .. } => "fetch",
            Self::RemoteUrl(_) => "ls-remote",
            Self::PublishRef { .. } => "update-ref",
            Self::Merge(_) => "merge",
        }
    }

    fn arguments(&self) -> Vec<OsString> {
        let mut arguments = self.suppressed_repository_code();
        arguments.extend(self.operation_arguments());
        arguments
    }

    /// Configuration that keeps an inspection from running repository code.
    ///
    /// `core.fsmonitor` names an executable Git runs to learn what the worktree
    /// changed, and it is not reached through `core.hooksPath`: pointing the
    /// hook search at the null device leaves it running. Observed on Git 2.50,
    /// it runs during `status` and during `fetch` alike, so a check described
    /// as reading would execute a repository-supplied command on the way. Every
    /// inspection therefore turns it off; Git falls back to reading the
    /// worktree itself, which is what an inspection is entitled to do.
    ///
    /// The fast-forward is deliberately absent. It hands Git the repository's
    /// own configuration — the same reason a smudge filter may run there — and
    /// the plan discloses the monitor alongside the hooks rather than silently
    /// suppressing something the user agreed to.
    fn suppressed_repository_code(&self) -> Vec<OsString> {
        match self {
            Self::Merge(_) => Vec::new(),
            _ => vec!["-c".into(), "core.fsmonitor=false".into()],
        }
    }

    fn operation_arguments(&self) -> Vec<OsString> {
        // `--dry-run` is what makes the fetch a read: Git transfers and
        // stores the objects and then skips every ref update, so no ref
        // transaction ever starts and a symbolic ref substituted for the
        // destination has nothing to redirect. `--porcelain` is how the
        // result comes back without a ref to read it from — one
        // machine-readable line per refspec naming the object the transport
        // reported (requires Git 2.41). No transaction also means no
        // `reference-transaction` hook, but the hook search is still pointed
        // at a path that holds none, so the claim does not rest on a flag's
        // behaviour staying put across Git versions.
        if let Self::Fetch {
            remote,
            refspec,
            hooks_path,
            ..
        } = self
        {
            let mut hooks_setting = OsString::from("core.hooksPath=");
            hooks_setting.push(hooks_path);
            return vec![
                "-c".into(),
                "gc.auto=0".into(),
                "-c".into(),
                hooks_setting,
                "fetch".into(),
                "--porcelain".into(),
                "--dry-run".into(),
                "--no-auto-maintenance".into(),
                // Even a dry run appends to FETCH_HEAD without this flag, and
                // rewriting FETCH_HEAD would discard state the user's own
                // fetch left there, which a check is not entitled to touch.
                "--no-write-fetch-head".into(),
                "--no-tags".into(),
                "--no-prune".into(),
                "--no-prune-tags".into(),
                "--recurse-submodules=no".into(),
                "--refmap=".into(),
                "--".into(),
                remote.into(),
                refspec.into(),
            ];
        }
        // A ref update runs the repository's `reference-transaction` hook, so
        // both of these point the hook search away for the same reason the
        // fetch does. `--no-deref` is the whole point of the pair: it writes
        // the named ref itself, so a ref substituted for a symbolic one cannot
        // redirect the update into whatever it points at. The expected old
        // value is the other half: the tracking ref is the user's, and an
        // ordinary `git fetch` or another Skilled process may have advanced it
        // while this one was fetching. Naming what was there makes Git refuse
        // rather than roll their newer value back to this reported one.
        if let Self::PublishRef {
            reference,
            revision,
            expected,
            hooks_path,
        } = self
        {
            let mut hooks_setting = OsString::from("core.hooksPath=");
            hooks_setting.push(hooks_path);
            return vec![
                "-c".into(),
                hooks_setting,
                "update-ref".into(),
                "--no-deref".into(),
                reference.into(),
                revision.into(),
                expected.into(),
            ];
        }
        // The default listing keeps the entry's mode, which is the whole point
        // here: `--name-only` says nothing about what kind of object is at the
        // path, and `-d` can only ever answer about a directory.
        if let Self::TreeEntryMode { revision, path } = self {
            return vec![
                "ls-tree".into(),
                "-z".into(),
                revision.into(),
                "--".into(),
                path.as_os_str().into(),
            ];
        }
        let strings: Vec<String> = match self {
            Self::RevParse(revision) => vec!["rev-parse".into(), revision.clone()],
            Self::AbsoluteGitDir => vec!["rev-parse".into(), "--absolute-git-dir".into()],
            Self::SymbolicHead => vec!["symbolic-ref".into(), "--quiet".into(), "HEAD".into()],
            Self::SymbolicRef(reference) => {
                vec!["symbolic-ref".into(), "--quiet".into(), reference.clone()]
            }
            Self::UpstreamRef(reference) => vec![
                "for-each-ref".into(),
                "--format=%(upstream)".into(),
                reference.clone(),
            ],
            // One process, both facts. Whether a ref is symbolic and what
            // object it names are the pair a publication has to decide over,
            // and reading them with two commands leaves a window in which a
            // ref can be direct for the first and symbolic for the second —
            // which is the state being refused.
            Self::RefState(reference) => vec![
                "for-each-ref".into(),
                "--format=%(objectname)%09%(symref)".into(),
                reference.clone(),
            ],
            Self::RemoteUrl(remote) => {
                vec!["ls-remote".into(), "--get-url".into(), remote.clone()]
            }
            Self::ConfigGet(key) => vec!["config".into(), "--get".into(), key.clone()],
            // `--show-scope` is what makes the answer usable: the same key is
            // ordinary in the user's own configuration and a refusal inside
            // the checkout, and only the scope tells them apart.
            // The value is read too, not just the name: several of these keys
            // have a documented spelling that turns the helper *off*, and a
            // repository that disables one names no program to run.
            Self::TransportSettings => vec![
                "config".into(),
                "--show-scope".into(),
                "--null".into(),
                "--get-regexp".into(),
                TRANSPORT_CODE_PATTERN.into(),
            ],
            // Which transports the user permits, read with the scope that says
            // the permission is theirs. The fetch's allowlist may only narrow
            // against this, never restore something it turned off.
            Self::TransportPolicy => vec![
                "config".into(),
                "--show-scope".into(),
                "--null".into(),
                "--get-regexp".into(),
                TRANSPORT_POLICY_PATTERN.into(),
            ],
            // Read the same shape as the refusal above, for the same reason:
            // the scope is what says whose command this is. A plain
            // `--get` would answer with the value Git would use, which is
            // precisely the checkout's own when the checkout has set one.
            Self::UserSshCommand => vec![
                "config".into(),
                "--show-scope".into(),
                "--null".into(),
                "--get-regexp".into(),
                SSH_COMMAND_PATTERN.into(),
            ],
            Self::FilterSettings => vec![
                "config".into(),
                "--null".into(),
                "--name-only".into(),
                "--get-regexp".into(),
                r"^filter\..*\.(clean|process|required)$".into(),
            ],
            // Every configured marker of a promisor remote, values included:
            // `remote.<name>.promisor` is a boolean and the other two name a
            // remote and a filter, so the value is what tells a live marker
            // from one that was turned off. Git lower-cases section and
            // variable names when it reports them, so the pattern is written
            // that way.
            Self::PromisorSettings => vec![
                "config".into(),
                "--null".into(),
                "--get-regexp".into(),
                r"^(extensions\.partialclone|remote\..*\.(promisor|partialclonefilter))$".into(),
            ],
            Self::CheckAttr => vec![
                "check-attr".into(),
                "--stdin".into(),
                "-z".into(),
                "filter".into(),
            ],
            Self::MergeBase(left, right) => {
                vec!["merge-base".into(), left.clone(), right.clone()]
            }
            Self::AheadBehind(left, right) => vec![
                "rev-list".into(),
                "--left-right".into(),
                "--count".into(),
                format!("{left}...{right}"),
            ],
            Self::ChangedPaths(left, right) => vec![
                "diff-tree".into(),
                "--no-commit-id".into(),
                "-r".into(),
                "-z".into(),
                "--raw".into(),
                "-M100%".into(),
                left.clone(),
                right.clone(),
            ],
            Self::CommitSummaries(left, right) => vec![
                "rev-list".into(),
                "--reverse".into(),
                "--format=%s".into(),
                "--no-commit-header".into(),
                format!("{left}..{right}"),
            ],
            Self::CatFile(revision) => {
                vec![
                    "cat-file".into(),
                    "-e".into(),
                    format!("{revision}^{{commit}}"),
                ]
            }
            Self::TreeEntryMode { .. } | Self::PublishRef { .. } => {
                unreachable!("tree and ref operations return above")
            }
            Self::Status => vec![
                "status".into(),
                "--porcelain=v1".into(),
                "-z".into(),
                "--untracked-files=all".into(),
                "--ignored=matching".into(),
                "--ignore-submodules=dirty".into(),
            ],
            Self::Fetch { .. } => unreachable!("fetch returns above"),
            // No signature flag either way. `merge.verifySignatures` is a
            // policy, and a policy Skilled has no standing to overrule: set in
            // the user's own global or system configuration it is theirs, and
            // passing `--no-verify-signatures` would fast-forward to an
            // unsigned tip that Git had been told to refuse. Re-reading the
            // object the preview named settles which object is merged, not who
            // vouches for it. What the setting can drag in — a `gpg.program`
            // the checkout names — is disclosed with the hooks, the monitor,
            // and the filters this write already runs, because disclosing what
            // may run is this operation's answer rather than suppressing it.
            Self::Merge(revision) => vec![
                "-c".into(),
                "maintenance.auto=false".into(),
                "merge".into(),
                "--ff-only".into(),
                "--no-squash".into(),
                "--no-autostash".into(),
                revision.clone(),
            ],
        };
        strings.into_iter().map(OsString::from).collect()
    }

    fn environment(&self) -> Vec<(OsString, OsString)> {
        match self {
            Self::Merge(_) => Vec::new(),
            // `GIT_ALLOW_PROTOCOL` is the one half of the transport claim the
            // fetch enforces for itself rather than inheriting from a reading
            // that came before it. Git treats it as `protocol.allow=never`
            // with each listed protocol allowed, and it overrides the
            // configuration outright — so a `remote.<name>.url`, an
            // `insteadOf` rewrite, or a `remote.<name>.vcs` written into the
            // checkout after the preflight refusal has read it cannot put a
            // helper behind this fetch. The preflight read still runs, because
            // refusing with `source.repository_transport_unsupported` says
            // more than a fetch failure does; what it no longer has to be is
            // the only thing standing between a config edit and a program.
            //
            // Because it overrides configuration outright, the list is
            // [`narrowed_transports`]'s answer rather than [`HANDLED_BY_GIT`]
            // itself: an inherited allowlist and the user's own `protocol.*`
            // policy each subtract from it, so enforcing a ceiling here cannot
            // hand back a transport somebody else had already refused.
            Self::Fetch {
                ssh_command,
                allowed_protocols,
                ..
            } => vec![
                ("GIT_TERMINAL_PROMPT".into(), "0".into()),
                ("GIT_ASKPASS".into(), "".into()),
                ("SSH_ASKPASS_REQUIRE".into(), "never".into()),
                ("GIT_SSH_COMMAND".into(), ssh_command.into()),
                ("GIT_ALLOW_PROTOCOL".into(), allowed_protocols.into()),
                ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
            ],
            Self::TreeEntryMode { .. } => vec![
                ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
                ("GIT_LITERAL_PATHSPECS".into(), "1".into()),
            ],
            _ => vec![("GIT_OPTIONAL_LOCKS".into(), "0".into())],
        }
    }
}

/// OpenSSH keeps the first value it obtains for most options. Insert the
/// mandatory non-interactive option immediately after the executable token so
/// a later user-supplied `BatchMode=no` cannot take precedence.
fn force_ssh_batch_mode(command: &str) -> Result<String> {
    let mut quote = None;
    let mut escaped = false;
    let end = command
        .char_indices()
        .find_map(|(index, character)| {
            if escaped {
                escaped = false;
                return None;
            }
            match (quote, character) {
                (_, '\\') => escaped = true,
                (None, '\'' | '"') => quote = Some(character),
                (Some(open), close) if open == close => quote = None,
                (None, value) if value.is_whitespace() => return Some(index),
                _ => {}
            }
            None
        })
        .unwrap_or(command.len());
    let executable = command[..end].trim_matches(['\'', '"']);
    if executable.is_empty()
        || matches!(executable, "env" | "exec" | "command")
        || executable.contains('=')
    {
        return Err(Error::InvalidGitOutput);
    }
    Ok(format!(
        "{} -o BatchMode=yes{}",
        &command[..end],
        &command[end..]
    ))
}

fn command(repository: &Path, op: &UpdateOp) -> Command {
    debug_assert!(UPDATE_SUBCOMMANDS.contains(&op.subcommand()));
    let mut command = Command::new("git");
    for key in REPOSITORY_ROUTING_ENVIRONMENT {
        command.env_remove(key);
    }
    if !matches!(op, UpdateOp::Merge(_)) {
        command.args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
        ]);
    }
    command.arg("-C").arg(repository).args(op.arguments());
    for (key, value) in op.environment() {
        command.env(key, value);
    }
    command
}

fn run(repository: &Path, op: UpdateOp) -> Result<Output> {
    command(repository, &op)
        .output()
        .map_err(Error::GitUnavailable)
}

fn run_cancellable(
    repository: &Path,
    op: &UpdateOp,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<Output>> {
    let child = command(repository, op)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(Error::GitUnavailable)?;
    collect_cancellable_child(child, cancelled, child_slot, None)
}

fn collect_cancellable_child(
    mut child: Child,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
    output_limit: Option<usize>,
) -> Result<Option<Output>> {
    let mut stdout = child.stdout.take().ok_or(Error::InvalidGitOutput)?;
    let mut stderr = child.stderr.take().ok_or(Error::InvalidGitOutput)?;
    let stdout_reader = std::thread::spawn(move || match output_limit {
        Some(limit) => read_bounded(&mut stdout, limit),
        None => {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        }
    });
    let stderr_reader = std::thread::spawn(move || match output_limit {
        Some(limit) => read_bounded(&mut stderr, limit),
        None => {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).map(|_| bytes)
        }
    });
    *child_slot
        .lock()
        .unwrap_or_else(|poison| poison.into_inner()) = Some(child);
    let status = loop {
        if cancelled.load(Ordering::Acquire) {
            if let Some(mut child) = child_slot
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .take()
            {
                terminate_child(&mut child);
            }
            drop(stdout_reader);
            drop(stderr_reader);
            return Ok(None);
        }
        let status = {
            let mut slot = child_slot
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let Some(child) = slot.as_mut() else {
                if cancelled.load(Ordering::Acquire) {
                    drop(stdout_reader);
                    drop(stderr_reader);
                    return Ok(None);
                }
                return Err(Error::InvalidGitOutput);
            };
            child.try_wait().map_err(Error::GitUnavailable)?
        };
        if let Some(status) = status {
            break status;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let _ = child_slot
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take();
    while !stdout_reader.is_finished() || !stderr_reader.is_finished() {
        if cancelled.load(Ordering::Acquire) {
            drop(stdout_reader);
            drop(stderr_reader);
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let stdout = stdout_reader
        .join()
        .map_err(|_| io::Error::other("Git stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| io::Error::other("Git stderr reader panicked"))??;
    Ok(Some(Output {
        status,
        stdout,
        stderr,
    }))
}

fn required_cancellable(
    repository: &Path,
    op: UpdateOp,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<Vec<u8>>> {
    let arguments = op.arguments();
    let Some(output) = run_cancellable(repository, &op, cancelled, child_slot)? else {
        return Ok(None);
    };
    if output.status.success() {
        Ok(Some(output.stdout))
    } else {
        Err(git_error(repository, &arguments, &output))
    }
}

fn optional_cancellable(
    repository: &Path,
    op: UpdateOp,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<Option<Vec<u8>>>> {
    Ok(run_cancellable(repository, &op, cancelled, child_slot)?
        .map(|output| output.status.success().then_some(output.stdout)))
}

fn required(repository: &Path, op: UpdateOp) -> Result<Vec<u8>> {
    let arguments = op.arguments();
    let output = run(repository, op)?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    Err(git_error(repository, &arguments, &output))
}

fn optional(repository: &Path, op: UpdateOp) -> Result<Option<Vec<u8>>> {
    let output = run(repository, op)?;
    Ok(output.status.success().then_some(output.stdout))
}

pub fn head_state(repository: &Path) -> Result<HeadState> {
    let revision = text(required(repository, UpdateOp::RevParse("HEAD".into()))?);
    let reference = optional(repository, UpdateOp::SymbolicHead)?
        .map(text)
        .filter(|value| !value.is_empty());
    Ok(match reference {
        Some(reference) => HeadState::Branch {
            reference,
            revision,
        },
        None => HeadState::Detached { revision },
    })
}

pub(crate) fn head_state_cancellable(
    repository: &Path,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<HeadState>> {
    let Some(revision) = required_cancellable(
        repository,
        UpdateOp::RevParse("HEAD".into()),
        cancelled,
        child_slot,
    )?
    else {
        return Ok(None);
    };
    let Some(reference) =
        optional_cancellable(repository, UpdateOp::SymbolicHead, cancelled, child_slot)?
    else {
        return Ok(None);
    };
    let revision = text(revision);
    let reference = reference.map(text).filter(|value| !value.is_empty());
    Ok(Some(match reference {
        Some(reference) => HeadState::Branch {
            reference,
            revision,
        },
        None => HeadState::Detached { revision },
    }))
}

pub(crate) fn repository_git_dir_cancellable(
    repository: &Path,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<PathBuf>> {
    let Some(bytes) =
        required_cancellable(repository, UpdateOp::AbsoluteGitDir, cancelled, child_slot)?
    else {
        return Ok(None);
    };
    let value = text(bytes);
    if value.is_empty() {
        return Err(Error::InvalidGitOutput);
    }
    Ok(Some(PathBuf::from(value)))
}

/// Resolve the configured upstream before a fetch. Returning `None` performs no
/// network operation and lets the classifier retain `source.no_upstream`.
pub fn upstream_of(repository: &Path, head: &HeadState) -> Result<Option<Upstream>> {
    let Some(reference) = head.reference() else {
        return Ok(None);
    };
    let branch = reference
        .strip_prefix("refs/heads/")
        .unwrap_or(reference)
        .to_owned();
    let remote = config_get(repository, &format!("branch.{branch}.remote"))?;
    let merge_ref = config_get(repository, &format!("branch.{branch}.merge"))?;
    let tracking_ref = optional(repository, UpdateOp::UpstreamRef(reference.into()))?
        .map(text)
        .filter(|value| !value.is_empty());
    let (Some(remote), Some(merge_ref), Some(tracking_ref)) = (remote, merge_ref, tracking_ref)
    else {
        return Ok(None);
    };
    if remote != "." && config_get(repository, &format!("remote.{remote}.url"))?.is_none() {
        return Ok(None);
    }
    if remote != "." && !tracking_ref.starts_with(&format!("refs/remotes/{remote}/")) {
        return Err(Error::InvalidGitOutput);
    }
    // An absent tracking ref leaves the revision unknown rather than the
    // upstream unconfigured; a local upstream (`remote = .`) has no fetch to
    // populate it, so there the absence is the end of the matter.
    let revision = optional(repository, UpdateOp::RevParse(tracking_ref.clone()))?
        .map(text)
        .filter(|value| !value.is_empty());
    if revision.is_none() && remote == "." {
        return Ok(None);
    }
    Ok(Some(Upstream {
        branch,
        remote,
        merge_ref,
        tracking_ref,
        revision,
    }))
}

pub(crate) fn upstream_of_cancellable(
    repository: &Path,
    head: &HeadState,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<Option<Upstream>>> {
    let Some(reference) = head.reference() else {
        return Ok(Some(None));
    };
    let branch = reference
        .strip_prefix("refs/heads/")
        .unwrap_or(reference)
        .to_owned();
    let Some(remote) = config_get_cancellable(
        repository,
        &format!("branch.{branch}.remote"),
        cancelled,
        child_slot,
    )?
    else {
        return Ok(None);
    };
    let Some(merge_ref) = config_get_cancellable(
        repository,
        &format!("branch.{branch}.merge"),
        cancelled,
        child_slot,
    )?
    else {
        return Ok(None);
    };
    let Some(tracking_ref) = optional_cancellable(
        repository,
        UpdateOp::UpstreamRef(reference.into()),
        cancelled,
        child_slot,
    )?
    else {
        return Ok(None);
    };
    let tracking_ref = tracking_ref.map(text).filter(|value| !value.is_empty());
    let (Some(remote), Some(merge_ref), Some(tracking_ref)) = (remote, merge_ref, tracking_ref)
    else {
        return Ok(Some(None));
    };
    if remote != "." {
        let Some(remote_url) = config_get_cancellable(
            repository,
            &format!("remote.{remote}.url"),
            cancelled,
            child_slot,
        )?
        else {
            return Ok(None);
        };
        if remote_url.is_none() || !tracking_ref.starts_with(&format!("refs/remotes/{remote}/")) {
            return if remote_url.is_none() {
                Ok(Some(None))
            } else {
                Err(Error::InvalidGitOutput)
            };
        }
    }
    let Some(revision) = optional_cancellable(
        repository,
        UpdateOp::RevParse(tracking_ref.clone()),
        cancelled,
        child_slot,
    )?
    else {
        return Ok(None);
    };
    // As in `upstream_of`: an absent tracking ref leaves the revision unknown,
    // and only a local upstream has no fetch that could supply it.
    let revision = revision.map(text).filter(|value| !value.is_empty());
    if revision.is_none() && remote == "." {
        return Ok(Some(None));
    }
    Ok(Some(Some(Upstream {
        branch,
        remote,
        merge_ref,
        tracking_ref,
        revision,
    })))
}

pub fn config_get(repository: &Path, key: &str) -> Result<Option<String>> {
    Ok(optional(repository, UpdateOp::ConfigGet(key.into()))?
        .map(text)
        .filter(|s| !s.is_empty()))
}

/// Whether Git would lazily fetch a missing object in this repository.
///
/// `extensions.partialClone` names the promisor remote a filtered clone was
/// made from, but it is not the only marker Git acts on: further promisor
/// remotes are configured as `remote.<name>.promisor`, and a filtered fetch
/// records `remote.<name>.partialCloneFilter`. Any of them lets an ordinary
/// read reach the network, which is exactly the access an explicit check
/// exists to bound, so all of them refuse the update rather than only the
/// extension key.
pub(crate) fn repository_is_partial_clone(repository: &Path) -> Result<bool> {
    let op = UpdateOp::PromisorSettings;
    let arguments = op.arguments();
    let output = run(repository, op)?;
    if !output.status.success() {
        if output.status.code() == Some(1) && output.stdout.is_empty() {
            return Ok(false);
        }
        return Err(git_error(repository, &arguments, &output));
    }
    parse_promisor_settings(output.stdout)
}

/// The repository-scoped settings that would make an inspection run a program
/// the registered checkout names.
///
/// Every one of these is an executable Git invokes on this machine during a
/// fetch: `core.sshCommand` and `core.askPass` are commands, `core.gitProxy`
/// is a proxy program, `core.alternateRefsCommand` is shell-executed to
/// advertise the tips of an alternate object store during negotiation, a
/// credential helper is a program per URL pattern,
/// `remote.<name>.uploadpack` is the program run for a local or SSH remote,
/// `remote.<name>.vcs` selects a `git-remote-<vcs>` helper, and
/// `protocol.<scheme>.command` is the helper an `ext::`-style URL runs.
///
/// The pattern is matched against the lower-cased names Git prints, which is
/// the spelling `--name-only` always produces regardless of how the file
/// spells them.
const TRANSPORT_CODE_PATTERN: &str = r"^(core\.(sshcommand|askpass|gitproxy|alternaterefscommand)|credential(\..*)?\.helper|remote\..*\.(uploadpack|vcs)|protocol(\..*)?\.command)$";

/// The transports Git reaches itself, without running a program the repository
/// named.
///
/// `ftp` and `ftps` are deprecated but Git fetches them through its bundled
/// curl support, so they are transports it implements rather than ones a
/// repository supplied. Everything absent from this list — `ext::` most
/// plainly, but any `git-remote-<name>` helper — is a program, and the fetch
/// is given the list as `GIT_ALLOW_PROTOCOL` rather than merely being checked
/// against it beforehand.
///
/// It is a ceiling and never a grant. `GIT_ALLOW_PROTOCOL` overrides every
/// `protocol.*` setting Git would otherwise read, so handing this list over
/// unconditionally would re-enable a transport the user had turned off — and
/// `protocol.file.allow=never` is a hardening people really do apply. What the
/// fetch is given is this list narrowed by the user's own policy and by any
/// `GIT_ALLOW_PROTOCOL` already in the environment; the checkout's scopes are
/// left out of that reading, so a repository can neither widen the ceiling nor
/// narrow someone else's fetch.
const HANDLED_BY_GIT: [&str; 7] = ["https", "http", "ssh", "git", "file", "ftp", "ftps"];

/// `protocol.allow` and every `protocol.<name>.allow`, which together say which
/// transports the user permits at all.
const TRANSPORT_POLICY_PATTERN: &str = r"^protocol\.(.*\.)?allow$";

/// The one transport setting Skilled reads out of the checkout itself, so the
/// one whose scope it can decide at the moment of use rather than in advance.
const SSH_COMMAND_PATTERN: &str = r"^core\.sshcommand$";

/// A transport setting the registered checkout itself configures, if any.
///
/// An explicit check is described to the user as reading. Git, though, takes
/// these settings from whatever configuration is in force, and a checkout the
/// user did not author carries its own — so pressing check on a repository
/// someone else prepared would run that repository's chosen program. Hooks and
/// `core.fsmonitor` are suppressed rather than refused because Git has a
/// documented way to turn each of them off; there is no equivalent for a
/// credential helper or an upload-pack program, and reconstructing the user's
/// own value to override with would be guessing at which scope they meant.
///
/// So the repository is refused instead, and only when *it* is the source of
/// the setting. Scope is the whole distinction: a value the user put in their
/// global or system configuration is theirs and keeps working, while the same
/// key inside the checkout blocks the check. `--show-scope` is what separates
/// them, and it reports a value pulled in by an `include.path` from the local
/// file as `local`, so a setting cannot be hidden behind an include.
pub(crate) fn repository_transport_code(repository: &Path) -> Result<Option<String>> {
    let op = UpdateOp::TransportSettings;
    let arguments = op.arguments();
    let output = run(repository, op)?;
    if !output.status.success() {
        // `--get-regexp` exits 1 with no output when nothing matched, which is
        // the ordinary answer rather than a failure.
        if output.status.code() == Some(1) && output.stdout.is_empty() {
            return Ok(None);
        }
        return Err(git_error(repository, &arguments, &output));
    }
    Ok(parse_transport_settings(output.stdout))
}

pub(crate) fn repository_transport_code_cancellable(
    repository: &Path,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<Option<String>>> {
    let op = UpdateOp::TransportSettings;
    let arguments = op.arguments();
    let Some(output) = run_cancellable(repository, &op, cancelled, child_slot)? else {
        return Ok(None);
    };
    if !output.status.success() {
        if output.status.code() == Some(1) && output.stdout.is_empty() {
            return Ok(Some(None));
        }
        return Err(git_error(repository, &arguments, &output));
    }
    Ok(Some(parse_transport_settings(output.stdout)))
}

/// The transport allowlist to hand this fetch, narrowed to what the caller and
/// the user already permit.
///
/// Three sources, and every one of them may only take a transport away:
/// [`HANDLED_BY_GIT`] is the ceiling, an inherited `GIT_ALLOW_PROTOCOL` is a
/// caller's boundary this process has no standing to widen, and the user's own
/// `protocol.*` policy is theirs the same way `merge.verifySignatures` is. The
/// checkout's scopes are read out of that last one, so a repository can neither
/// grant itself a transport nor deny the user one.
fn permitted_transports(repository: &Path) -> Result<String> {
    Ok(narrowed_transports(
        &transport_policy(repository)?,
        &inherited_transport_boundary(),
    ))
}

fn permitted_transports_cancellable(
    repository: &Path,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<String>> {
    let Some(policy) = transport_policy_cancellable(repository, cancelled, child_slot)? else {
        return Ok(None);
    };
    Ok(Some(narrowed_transports(
        &policy,
        &inherited_transport_boundary(),
    )))
}

fn transport_policy(repository: &Path) -> Result<TransportPolicy> {
    let op = UpdateOp::TransportPolicy;
    let arguments = op.arguments();
    let output = run(repository, op)?;
    if !output.status.success() {
        // Nothing configured anywhere, so every protocol keeps Git's own
        // default.
        if output.status.code() == Some(1) && output.stdout.is_empty() {
            return Ok(TransportPolicy::default());
        }
        return Err(git_error(repository, &arguments, &output));
    }
    Ok(parse_transport_policy(output.stdout))
}

fn transport_policy_cancellable(
    repository: &Path,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<TransportPolicy>> {
    let op = UpdateOp::TransportPolicy;
    let arguments = op.arguments();
    let Some(output) = run_cancellable(repository, &op, cancelled, child_slot)? else {
        return Ok(None);
    };
    if !output.status.success() {
        if output.status.code() == Some(1) && output.stdout.is_empty() {
            return Ok(Some(TransportPolicy::default()));
        }
        return Err(git_error(repository, &arguments, &output));
    }
    Ok(Some(parse_transport_policy(output.stdout)))
}

/// The user's own `protocol.*` settings, keyed as Git spells them.
///
/// `protocol.<name>.allow` decides a single transport and `protocol.allow` is
/// the default for the rest; both are scalars whose last value wins, and both
/// are read only from the scopes that are the user's.
#[derive(Debug, Default)]
struct TransportPolicy(std::collections::BTreeMap<String, String>);

/// What Git does with a transport, spelled the way `protocol.<name>.allow` is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransportVerdict {
    Always,
    /// Permitted only when the operation came from the user rather than from
    /// Git recursing into one — `GIT_PROTOCOL_FROM_USER` is what says which.
    UserOnly,
    Never,
    /// A value Git does not recognise, which makes it abort rather than pick a
    /// side. Nothing here can abort a user's fetch on that account, so it
    /// refuses the transport instead: a policy that cannot be read is not a
    /// policy that permits.
    Unreadable,
}

impl TransportPolicy {
    /// Resolved exactly as Git resolves it: the per-protocol key, then the
    /// blanket default, then Git's own built-in for that protocol.
    fn verdict(&self, protocol: &str) -> TransportVerdict {
        self.0
            .get(&format!("protocol.{protocol}.allow"))
            .or_else(|| self.0.get("protocol.allow"))
            .map_or_else(
                || Self::builtin(protocol),
                |value| match value.as_str() {
                    "always" => TransportVerdict::Always,
                    "user" => TransportVerdict::UserOnly,
                    "never" => TransportVerdict::Never,
                    _ => TransportVerdict::Unreadable,
                },
            )
    }

    /// Git's built-in policy for the transports it implements. `http`, `https`,
    /// `git`, and `ssh` are its "known safe" set; everything else it does not
    /// name falls to user-only, which is where `file`, `ftp`, and `ftps` land.
    fn builtin(protocol: &str) -> TransportVerdict {
        match protocol {
            "http" | "https" | "git" | "ssh" => TransportVerdict::Always,
            _ => TransportVerdict::UserOnly,
        }
    }
}

fn parse_transport_policy(bytes: Vec<u8>) -> TransportPolicy {
    let mut records = bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .map(String::from_utf8_lossy);
    let mut policy = std::collections::BTreeMap::new();
    while let (Some(scope), Some(entry)) = (records.next(), records.next()) {
        if matches!(scope.as_ref(), "local" | "worktree") {
            continue;
        }
        let (name, value) = entry
            .split_once('\n')
            .map_or((entry.as_ref(), ""), |(name, value)| (name, value));
        // Lowered but not trimmed. Git compares these case-insensitively and
        // whole: a quoted `"always "` keeps its space through the config
        // parser and makes Git abort, so trimming it here would turn a policy
        // Git refuses to read into permission.
        policy.insert(name.to_owned(), value.to_ascii_lowercase());
    }
    TransportPolicy(policy)
}

/// The boundary the environment this process was started in already draws.
///
/// Two variables, read the way Git reads them. `GIT_ALLOW_PROTOCOL` is an
/// allowlist a caller set, and `GIT_PROTOCOL_FROM_USER=0` says this is not a
/// user-initiated operation, which is what makes Git's `user` policy a refusal
/// — including for `file`, `ftp`, and `ftps`, whose built-in policy that is.
struct TransportBoundary {
    allowed: Option<std::ffi::OsString>,
    from_user: bool,
}

fn inherited_transport_boundary() -> TransportBoundary {
    TransportBoundary {
        allowed: env::var_os("GIT_ALLOW_PROTOCOL"),
        from_user: git_environment_boolean(env::var_os("GIT_PROTOCOL_FROM_USER").as_deref()),
    }
}

/// A boolean spelled the way Git spells one in the environment.
///
/// Absent is true, which is Git's default for `GIT_PROTOCOL_FROM_USER`. Then
/// the textual spellings, and then any integer, where every way of writing
/// zero — `0`, `00`, `+0`, `-0` — is false. Git aborts on anything else, and
/// nothing here is entitled to abort a user's fetch, so an unreadable value
/// reads as false instead: that only ever narrows the allowlist, which is the
/// single direction a misreading may go.
fn git_environment_boolean(value: Option<&std::ffi::OsStr>) -> bool {
    let Some(value) = value else {
        return true;
    };
    let Some(value) = value.to_str() else {
        return false;
    };
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" => true,
        "" | "false" | "no" | "off" => false,
        number => number.parse::<i64>().is_ok_and(|number| number != 0),
    }
}

/// [`HANDLED_BY_GIT`] with everything the user's policy or the caller's
/// environment already refuses taken out of it.
///
/// An empty result is a real answer rather than a fallback: it says every
/// transport this fetch could use is one somebody turned off, and Git refusing
/// the fetch is the honest outcome. Widening back to the full list there would
/// be the bypass this exists to prevent — which is also why an inherited
/// allowlist this cannot read leaves nothing rather than everything.
///
/// The inherited list is compared the way Git compares it: split on `:` and
/// matched exactly, with no trimming and no case folding, because `HTTPS` and
/// ` https` are entries Git would not match either and treating them as
/// `https` would grant a transport the caller did not.
fn narrowed_transports(policy: &TransportPolicy, boundary: &TransportBoundary) -> String {
    let inherited = match &boundary.allowed {
        None => None,
        Some(value) => match value.to_str() {
            Some(value) => Some(value.split(':').collect::<Vec<_>>()),
            None => return String::new(),
        },
    };
    HANDLED_BY_GIT
        .iter()
        .filter(|protocol| match policy.verdict(protocol) {
            TransportVerdict::Always => true,
            TransportVerdict::UserOnly => boundary.from_user,
            TransportVerdict::Never | TransportVerdict::Unreadable => false,
        })
        .filter(|protocol| {
            inherited
                .as_ref()
                .is_none_or(|allowed| allowed.contains(protocol))
        })
        .copied()
        .collect::<Vec<_>>()
        .join(":")
}

/// The `core.sshCommand` the *user* configured, ignoring the checkout's own.
///
/// A repository-scoped value here is already a refusal, but that refusal is
/// read once and the fetch is spawned several processes later — and this is
/// the value Skilled itself exports as `GIT_SSH_COMMAND`, so a command written
/// into the checkout in between would be read here and run. Asking for the
/// scope alongside the value closes that rather than narrowing it: what this
/// returns is the user's own by construction, whatever the checkout does
/// meanwhile. The check still refuses a repository-scoped value, because a
/// checkout that names a program is a thing to report and not merely to
/// disregard.
fn user_ssh_command(repository: &Path) -> Result<Option<String>> {
    let op = UpdateOp::UserSshCommand;
    let arguments = op.arguments();
    let output = run(repository, op)?;
    if !output.status.success() {
        // Nothing configured anywhere, which is the ordinary answer.
        if output.status.code() == Some(1) && output.stdout.is_empty() {
            return Ok(None);
        }
        return Err(git_error(repository, &arguments, &output));
    }
    Ok(parse_user_ssh_command(output.stdout))
}

fn user_ssh_command_cancellable(
    repository: &Path,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<Option<String>>> {
    let op = UpdateOp::UserSshCommand;
    let arguments = op.arguments();
    let Some(output) = run_cancellable(repository, &op, cancelled, child_slot)? else {
        return Ok(None);
    };
    if !output.status.success() {
        if output.status.code() == Some(1) && output.stdout.is_empty() {
            return Ok(Some(None));
        }
        return Err(git_error(repository, &arguments, &output));
    }
    Ok(Some(parse_user_ssh_command(output.stdout)))
}

/// The surviving `core.sshCommand` from the scopes that are the user's.
///
/// The records alternate the same way [`parse_transport_settings`] reads them:
/// a scope, then the key and its value separated by a newline. `core.sshCommand`
/// is a scalar, so the last value Git read wins — with the checkout's own
/// scopes struck out rather than allowed to be that last value. An empty value
/// is the documented way to configure nothing, and reads as nothing here.
fn parse_user_ssh_command(bytes: Vec<u8>) -> Option<String> {
    let mut records = bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .map(String::from_utf8_lossy);
    let mut command = None;
    while let (Some(scope), Some(entry)) = (records.next(), records.next()) {
        if matches!(scope.as_ref(), "local" | "worktree") {
            continue;
        }
        let value = entry.split_once('\n').map_or("", |(_, value)| value);
        command = Some(value.trim().to_owned());
    }
    command.filter(|command| !command.is_empty())
}

/// The first repository-scoped setting that actually names a program.
///
/// The records alternate: a scope, then the key and its value separated by a
/// newline, each record ended by a NUL. Only `local` and `worktree` are the
/// repository's own; `global`, `system`, and the `command` and `unknown`
/// scopes Git uses for `-c` arguments and environment overrides are the user's
/// own configuration and are left alone.
///
/// A key set to a documented disabling value is not a refusal. An empty
/// `credential.helper` resets the helper list rather than adding to it, an
/// empty `core.askPass` and `core.sshCommand` leave Git with nothing to run,
/// and `core.gitProxy` spells its own disabling value `none`. Refusing those
/// would block a repository that has explicitly turned the helper *off*.
fn parse_transport_settings(bytes: Vec<u8>) -> Option<String> {
    let mut records = bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .map(String::from_utf8_lossy);
    // Grouped by key, in the order Git read them: system, then global, then
    // local, then worktree, and within each file the order the file gives.
    // That order is what decides which value survives, so the decision cannot
    // be made one record at a time.
    let mut settings: std::collections::BTreeMap<String, Vec<(String, String)>> =
        std::collections::BTreeMap::new();
    while let (Some(scope), Some(entry)) = (records.next(), records.next()) {
        let (name, value) = entry
            .split_once('\n')
            .map_or((entry.as_ref(), ""), |(name, value)| (name, value));
        settings
            .entry(name.to_ascii_lowercase())
            .or_default()
            .push((scope.into_owned(), value.trim().to_owned()));
    }
    settings
        .into_iter()
        .find(|(name, entries)| effective_transport_entry(name, entries))
        .map(|(name, _)| name)
}

/// Whether the value that survives for `name` is a program the repository
/// chose.
///
/// A later record can undo an earlier one, so reporting the first repository
/// value that looks like a command would block repositories that go on to
/// disable it. Git resolves these three ways, and all three are applied here.
///
/// A credential helper is a *list* that an empty value resets, so only the
/// entries after the last reset are live. `core.gitProxy` is neither a list
/// nor a scalar: it may appear many times, each optionally suffixed `for
/// DOMAIN`, and Git uses the *first* entry whose domain matches — so a proxy
/// command aimed at the upstream host followed by an unconditional `none`
/// leaves the command in force for that host. Modelling that matching would
/// mean deciding which host the fetch resolves to, so every applicable entry
/// is treated as live instead and any repository-scoped proxy command refuses.
/// Everything else is a scalar whose last value wins outright.
///
/// Either way, a live entry is a refusal only when it is the repository's own.
fn effective_transport_entry(name: &str, entries: &[(String, String)]) -> bool {
    let live: &[(String, String)] = if is_credential_helper(name) {
        let reset = entries
            .iter()
            .rposition(|(_, value)| value.is_empty())
            .map_or(0, |index| index + 1);
        &entries[reset..]
    } else if name.eq_ignore_ascii_case("core.gitproxy") {
        entries
    } else {
        entries.last().map_or(&[][..], std::slice::from_ref)
    };
    live.iter().any(|(scope, value)| {
        matches!(scope.as_str(), "local" | "worktree")
            && !transport_setting_is_disabled(name, value)
    })
}

/// Whether `name` is a credential helper, which Git accumulates as a list
/// rather than overwriting.
fn is_credential_helper(name: &str) -> bool {
    name.starts_with("credential.") && name.ends_with(".helper") || name == "credential.helper"
}

/// Whether a transport setting's value is the documented way to turn it off.
fn transport_setting_is_disabled(name: &str, value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    // `core.gitProxy` reads `none` as "connect directly"; the other keys have
    // no non-empty disabling spelling, so any value is a program.
    name.eq_ignore_ascii_case("core.gitproxy") && value.eq_ignore_ascii_case("none")
}

/// The URL a fetch of `remote` would actually use.
///
/// Asking `git config --get remote.<name>.url` answers a different question in
/// two ways, and a checkout can hide behind either: `--get` returns the *last*
/// configured value while a fetch uses the *first*, and `url.<base>.insteadOf`
/// rewrites the value on the way to the transport, so a remote can present an
/// ordinary HTTPS address to a naive read and still be fetched as `ext::`.
/// `ls-remote --get-url` applies the rewrites and reports what Git would use,
/// and it is documented to exit without contacting the remote.
pub(crate) fn effective_remote_url(repository: &Path, remote: &str) -> Result<Option<String>> {
    let op = UpdateOp::RemoteUrl(remote.to_owned());
    let arguments = op.arguments();
    let output = run(repository, op)?;
    if !output.status.success() {
        return Err(git_error(repository, &arguments, &output));
    }
    let url = text(output.stdout);
    Ok((!url.is_empty()).then_some(url))
}

pub(crate) fn effective_remote_url_cancellable(
    repository: &Path,
    remote: &str,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<Option<String>>> {
    let op = UpdateOp::RemoteUrl(remote.to_owned());
    let arguments = op.arguments();
    let Some(output) = run_cancellable(repository, &op, cancelled, child_slot)? else {
        return Ok(None);
    };
    if !output.status.success() {
        return Err(git_error(repository, &arguments, &output));
    }
    let url = text(output.stdout);
    Ok(Some((!url.is_empty()).then_some(url)))
}

/// Whether fetching this URL would run a program the checkout selected.
///
/// Git reaches a remote through a transport helper — an executable named
/// `git-remote-<transport>` — whenever the URL names one, either as
/// `<transport>::<address>` or as an unrecognised `<scheme>://`. `ext::` is
/// the extreme case, running the rest of the URL as a command outright, but
/// any helper is a program the repository chose rather than one Git
/// implements. So this is an allowlist of the transports Git handles itself,
/// not a denylist of the two spellings that are obviously dangerous: a new
/// helper must be added deliberately rather than slip through unrecognised.
///
/// A `::` that appears after a slash is part of a path rather than a
/// transport, and a plain path or an scp-style `user@host:path` names no
/// scheme at all.
///
/// The same list is handed to the fetch as `GIT_ALLOW_PROTOCOL`, so this
/// answer decides the refusal's wording while Git enforces it. That is also
/// why the scheme is matched exactly rather than case-insensitively: URL
/// schemes are case-insensitive to read but Git is not one of their readers.
/// It compares the spelling literally, so `FILE://` is not the `file`
/// transport it implements — it is `git-remote-FILE`, a helper, and observably
/// runs as one. Folding the case here would call that built-in and let the
/// preflight wave through the very thing it exists to refuse.
pub(crate) fn remote_url_runs_a_helper(url: &str) -> bool {
    let url = url.trim();
    if let Some(index) = url.find("::")
        && !url[..index].contains('/')
    {
        return true;
    }
    if let Some((scheme, _)) = url.split_once("://")
        && !scheme.contains('/')
    {
        return !HANDLED_BY_GIT.contains(&scheme);
    }
    false
}

pub(crate) fn repository_is_partial_clone_cancellable(
    repository: &Path,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<bool>> {
    let op = UpdateOp::PromisorSettings;
    let arguments = op.arguments();
    let Some(output) = run_cancellable(repository, &op, cancelled, child_slot)? else {
        return Ok(None);
    };
    if !output.status.success() {
        if output.status.code() == Some(1) && output.stdout.is_empty() {
            return Ok(Some(false));
        }
        return Err(git_error(repository, &arguments, &output));
    }
    parse_promisor_settings(output.stdout).map(Some)
}

/// Whether any reported promisor marker is in force.
///
/// `--null` separates records with a NUL and a key from its value with a
/// newline; a key set without a value has no newline at all, which Git reads
/// as a true boolean. `remote.<name>.promisor` is the only boolean of the
/// three, so it is the only one a value can switch off; a marker whose value
/// Git would reject is treated as in force rather than ignored.
fn parse_promisor_settings(bytes: Vec<u8>) -> Result<bool> {
    for record in bytes.split(|byte| *byte == 0) {
        if record.is_empty() {
            continue;
        }
        let record = String::from_utf8(record.to_vec()).map_err(|_| Error::InvalidGitOutput)?;
        let (key, value) = record
            .split_once('\n')
            .map_or((record.as_str(), None), |(key, value)| (key, Some(value)));
        let disabled = key.ends_with(".promisor")
            && value.is_some_and(|value| {
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "false" | "no" | "off" | "0" | ""
                )
            });
        if !disabled {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn config_get_cancellable(
    repository: &Path,
    key: &str,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<Option<String>>> {
    Ok(optional_cancellable(
        repository,
        UpdateOp::ConfigGet(key.into()),
        cancelled,
        child_slot,
    )?
    .map(|value| value.map(text).filter(|text| !text.is_empty())))
}

/// Refuse a fetch whose destination Git would resolve to something else.
///
/// A ref transaction dereferences a symbolic ref before updating it, so a
/// `refs/remotes/<remote>/<branch>` that is symbolic moves its referent rather
/// than itself — and nothing stops that referent from being a local branch,
/// which a forced refspec would then rewrite during a check the user was told
/// only reads. The name is checked here, immediately before the fetch that
/// would use it, and the refusal happens before any network access.
fn reject_symbolic_tracking_ref(repository: &Path, upstream: &Upstream) -> Result<()> {
    let op = UpdateOp::SymbolicRef(upstream.tracking_ref.clone());
    let arguments = op.arguments();
    let output = run(repository, op)?;
    symbolic_tracking_ref_verdict(repository, upstream, &arguments, &output)
}

fn reject_symbolic_tracking_ref_cancellable(
    repository: &Path,
    upstream: &Upstream,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<()>> {
    let op = UpdateOp::SymbolicRef(upstream.tracking_ref.clone());
    let arguments = op.arguments();
    let Some(output) = run_cancellable(repository, &op, cancelled, child_slot)? else {
        return Ok(None);
    };
    symbolic_tracking_ref_verdict(repository, upstream, &arguments, &output).map(Some)
}

/// `symbolic-ref --quiet` succeeds only for a symbolic ref, and exits 1 both
/// for a direct ref and for one that does not exist yet; neither of those can
/// send the update anywhere but the named destination.
fn symbolic_tracking_ref_verdict(
    repository: &Path,
    upstream: &Upstream,
    arguments: &[OsString],
    output: &Output,
) -> Result<()> {
    if output.status.success() {
        return Err(Error::SymbolicTrackingRef {
            reference: upstream.tracking_ref.clone(),
            target: text(output.stdout.clone()),
        });
    }
    if output.status.code() == Some(1) {
        return Ok(());
    }
    Err(git_error(repository, arguments, output))
}

/// Where a fetch is told to look for hooks: the platform's null device, which
/// is not a directory, so Git finds no hook under it.
///
/// Outside the repository on purpose. A name inside the Git directory would be
/// a path the checkout can carry — a checkout is not always one this machine
/// cloned — and pointing the hook search at it would run whatever was waiting
/// there, which is the thing being prevented.
#[cfg(unix)]
const SUPPRESSED_HOOKS_PATH: &str = "/dev/null";
#[cfg(windows)]
const SUPPRESSED_HOOKS_PATH: &str = "NUL";

/// Where a fetch destination name nests. The name exists in the refspec and
/// in the porcelain report, and nowhere else.
const FETCH_DESTINATION_NAMESPACE: &str = "refs/skilled/fetch";

/// A fetch destination that is only ever a name.
///
/// Git dereferences a symbolic ref when a transaction updates it, so a fetch
/// that writes any ref writes wherever that ref's name points — a local
/// branch included, if a symbolic ref is substituted for the destination
/// between any check and Git's own transaction. An earlier revision of this
/// check narrowed that race by fetching into a per-invocation staging ref,
/// deleted before and after and re-checked in between; `skilled-q59` records
/// why a name that is checked and then written stays reachable. The
/// destination is now never written at all: the fetch runs with `--dry-run`,
/// which stores the objects and then skips every ref update, and the object
/// it obtained comes back through `--porcelain`'s report instead of a ref.
/// A name no process writes cannot be redirected, whatever is standing at
/// it — a direct ref, a symbolic ref, or a ref occupying the namespace
/// itself, each observed inert under a dry run on Git 2.50. That last case is
/// also why the namespace needs no occupancy probe or fallback spelling: a
/// directory/file conflict only exists for a write.
///
/// The name is still unique per invocation, for the report rather than for
/// safety: the porcelain line is matched by destination, and a destination no
/// other party prepared cannot already hold the incoming object — which is
/// the one documented way a refspec earns no report line, and so a state
/// [`reported_revision`]'s caller may treat as tampering rather than routine.
fn fetch_destination() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{FETCH_DESTINATION_NAMESPACE}/{:x}-{nanos:x}-{:x}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

/// The object the porcelain report names for `destination`, if exactly one
/// plausible line does.
///
/// `--porcelain` prints one `<flag> <old-oid> <new-oid> <local-reference>`
/// line per refspec. The flag is a single character that may itself be a
/// space, so the line is read from its end rather than its start: the last
/// field is the destination and the field before it the incoming object. The
/// old object is ignored — when something is standing at the destination the
/// dry run reads it, possibly through a substituted symbolic ref, and evidence
/// obtained that way must not shape the result. The incoming object must
/// spell a full object name that is not the all-zero absent marker, because
/// the caller is about to publish it to the user's remote-tracking ref.
fn reported_revision(stdout: &[u8], destination: &str) -> Option<String> {
    let report = String::from_utf8_lossy(stdout);
    let mut revisions = report.lines().filter_map(|line| {
        let mut fields = line.split_whitespace().rev();
        (fields.next() == Some(destination))
            .then(|| fields.next())
            .flatten()
    });
    let revision = revisions.next()?;
    if revisions.next().is_some() || !full_object_name(revision) {
        return None;
    }
    Some(revision.to_owned())
}

/// Whether `revision` spells out an entire object name: every byte hex, at a
/// documented object-name length, and not the all-zero marker Git uses for a
/// ref that is absent.
fn full_object_name(revision: &str) -> bool {
    matches!(revision.len(), 40 | 64)
        && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
        && revision.bytes().any(|byte| byte != b'0')
}

/// The object a *direct* ref names: `None` when the ref does not exist, and
/// `None` when it is symbolic, because a symbolic ref's object is its
/// referent's rather than its own.
///
/// Both facts come out of one Git process. Asking whether a ref is symbolic
/// and asking what it holds in two commands leaves a window in which a ref can
/// be direct for the first answer and symbolic for the second, and a value
/// read through a substituted symbolic ref is the referent's — exactly the
/// state the publication is refusing.
fn direct_ref_object(repository: &Path, reference: &str) -> Result<Option<String>> {
    let output = run(repository, UpdateOp::RefState(reference.to_owned()))?;
    Ok(parse_direct_ref_object(&output))
}

fn direct_ref_object_cancellable(
    repository: &Path,
    reference: &str,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<Option<String>>> {
    let op = UpdateOp::RefState(reference.to_owned());
    let Some(output) = run_cancellable(repository, &op, cancelled, child_slot)? else {
        return Ok(None);
    };
    Ok(Some(parse_direct_ref_object(&output)))
}

fn parse_direct_ref_object(output: &Output) -> Option<String> {
    if !output.status.success() {
        return None;
    }
    let line = text(output.stdout.clone());
    let (object, symref) = line.split_once('\t')?;
    // A ref that names a referent is symbolic, whatever object that referent
    // happens to resolve to.
    if !symref.is_empty() || object.is_empty() {
        return None;
    }
    Some(object.to_owned())
}

/// Publish `revision` to `reference`, tolerating a publication that got there
/// first with the very same object.
///
/// The compare-and-swap exists to stop this fetch rolling a newer value back,
/// and a ref already holding exactly what the fetch reported is not that: another
/// `git fetch` or another Skilled process reached the same upstream commit and
/// wrote it. Reporting a fetch failure there would cache a blocker describing
/// nothing that is wrong, and — because the later run holds the later
/// generation — that blocker would displace the successful result.
///
/// [`direct_ref_object`] is what makes the tolerance safe: it reports nothing
/// for a symbolic ref, so a ref substituted for one whose referent happens to
/// hold `revision` is not mistaken for a ref that holds it.
fn publish_or_accept_identical(
    repository: &Path,
    upstream: &Upstream,
    revision: &str,
    expected: Option<&str>,
) -> Result<()> {
    let Err(error) = publish_ref(repository, &upstream.tracking_ref, revision, expected) else {
        return Ok(());
    };
    if direct_ref_object(repository, &upstream.tracking_ref)?.as_deref() == Some(revision) {
        return Ok(());
    }
    Err(error)
}

/// The old value that says "this ref must not exist".
const ABSENT_REF: &str = "0000000000000000000000000000000000000000";

fn publish_ref_op(reference: &str, revision: &str, expected: Option<&str>) -> UpdateOp {
    UpdateOp::PublishRef {
        reference: reference.to_owned(),
        revision: revision.to_owned(),
        expected: expected.unwrap_or(ABSENT_REF).to_owned(),
        hooks_path: SUPPRESSED_HOOKS_PATH.into(),
    }
}

fn publish_ref(
    repository: &Path,
    reference: &str,
    revision: &str,
    expected: Option<&str>,
) -> Result<()> {
    let op = publish_ref_op(reference, revision, expected);
    let arguments = op.arguments();
    let output = run(repository, op)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(git_error(repository, &arguments, &output))
    }
}

fn publish_ref_cancellable(
    repository: &Path,
    reference: &str,
    revision: &str,
    expected: Option<&str>,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<std::result::Result<(), Error>>> {
    let op = publish_ref_op(reference, revision, expected);
    let arguments = op.arguments();
    let Some(output) = run_cancellable(repository, &op, cancelled, child_slot)? else {
        return Ok(None);
    };
    Ok(Some(if output.status.success() {
        Ok(())
    } else {
        Err(git_error(repository, &arguments, &output))
    }))
}

/// What the fetch may run and reach, settled before it starts.
///
/// Both are decided here rather than left to Git's reading of the repository
/// configuration, which is the whole point: these are the two halves of the
/// transport claim a checkout could otherwise rewrite between the refusal that
/// vetted it and the process that acts on it.
struct FetchTransport {
    ssh_command: String,
    allowed_protocols: String,
}

fn fetch_op(upstream: &Upstream, transport: FetchTransport, destination: &str) -> UpdateOp {
    UpdateOp::Fetch {
        remote: upstream.remote.clone(),
        // The explicit refspec fetches this branch even when the user's
        // ordinary fetch refspec excludes it. The leading `+` is required to
        // observe and classify a rewritten upstream as diverged.
        refspec: upstream_refspec(upstream, destination),
        ssh_command: transport.ssh_command,
        allowed_protocols: transport.allowed_protocols,
        hooks_path: SUPPRESSED_HOOKS_PATH.into(),
    }
}

/// Check the configured upstream and publish what it holds, without the fetch
/// writing any ref of its own.
///
/// The publication that follows the fetch is the one ref write the check
/// performs, and it is confined by construction: `update-ref --no-deref`
/// writes the named remote-tracking ref itself, never a referent, so no
/// substitution can send the write outside that name. What `--no-deref`
/// cannot do — observed across `update`, guarded delete, and must-not-exist
/// creation alike on Git 2.50 — is refuse a symbolic ref: every form resolves
/// the ref's referent to answer the old-value check and then replaces the
/// symbolic ref in place with a direct one. Git offers no single operation
/// that asserts a ref's kind and its value together, so the kind check is a
/// separate process immediately before the write, twice over: a tracking ref
/// that is symbolic when the check starts, or when the publication is
/// reached, is refused untouched. What remains is a ref made symbolic inside
/// the final check-to-write gap. Such a ref is replaced rather than refused —
/// its referent still never written — and since a symbolic ref standing at
/// check start refuses the whole check, the ref lost that way existed only
/// for the seconds this check was running. That residual, and the argument
/// for accepting it, is the `skilled-q59` closeout record.
pub fn fetch_upstream(repository: &Path, upstream: &Upstream) -> Result<String> {
    reject_symbolic_tracking_ref(repository, upstream)?;
    let transport = FetchTransport {
        ssh_command: force_ssh_batch_mode(
            &env::var("GIT_SSH_COMMAND")
                .ok()
                .or(user_ssh_command(repository)?)
                .unwrap_or_else(|| "ssh".into()),
        )?,
        allowed_protocols: permitted_transports(repository)?,
    };
    // Read before the fetch, so the publication below can say what it expected
    // to be replacing rather than overwriting whatever arrived meanwhile.
    let before = direct_ref_object(repository, &upstream.tracking_ref)?;
    let revision = fetch_reported_revision(repository, upstream, transport)?;
    // Refused rather than clobbered: a tracking ref the user made symbolic
    // is theirs, and `--no-deref` would overwrite it in place. The check
    // is for that case; the flag is what makes losing the race harmless to
    // everything but the substituted ref itself.
    reject_symbolic_tracking_ref(repository, upstream)?;
    publish_or_accept_identical(repository, upstream, &revision, before.as_deref())?;
    Ok(revision)
}

/// Fetch the upstream branch's objects and return the commit the transport
/// reported, leaving every ref exactly as it was.
fn fetch_reported_revision(
    repository: &Path,
    upstream: &Upstream,
    transport: FetchTransport,
) -> Result<String> {
    let destination = fetch_destination();
    let op = fetch_op(upstream, transport, &destination);
    let arguments = op.arguments();
    let output = run_bounded(repository, &op, MAX_FETCH_OUTPUT_BYTES)?;
    if !output.status.success() {
        return Err(git_error(repository, &arguments, &output));
    }
    let Some(revision) = reported_revision(&output.stdout, &destination) else {
        return Err(Error::FetchUnreported);
    };
    // The report is the transport's word; the object store is the evidence. A
    // dry run stores what it fetched, so a reported commit that cannot be
    // read back was never actually obtained.
    if !commit_exists(repository, &revision)? {
        return Err(Error::FetchedCommitMissing { revision });
    }
    Ok(revision)
}

/// Fetch on a worker thread while publishing the child for main-thread
/// cancellation. `None` is a clean cancellation, not a fetch failure.
pub(crate) fn fetch_upstream_cancellable(
    repository: &Path,
    upstream: &Upstream,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<String>> {
    if reject_symbolic_tracking_ref_cancellable(repository, upstream, cancelled, child_slot)?
        .is_none()
    {
        return Ok(None);
    }
    let configured_ssh = if env::var("GIT_SSH_COMMAND").is_ok() {
        env::var("GIT_SSH_COMMAND").ok()
    } else {
        let Some(value) = user_ssh_command_cancellable(repository, cancelled, child_slot)? else {
            return Ok(None);
        };
        value
    };
    let Some(allowed_protocols) =
        permitted_transports_cancellable(repository, cancelled, child_slot)?
    else {
        return Ok(None);
    };
    let transport = FetchTransport {
        ssh_command: force_ssh_batch_mode(configured_ssh.as_deref().unwrap_or("ssh"))?,
        allowed_protocols,
    };
    let Some(before) =
        direct_ref_object_cancellable(repository, &upstream.tracking_ref, cancelled, child_slot)?
    else {
        return Ok(None);
    };
    let Some(revision) = fetch_reported_revision_cancellable(
        repository, upstream, transport, cancelled, child_slot,
    )?
    else {
        return Ok(None);
    };
    if reject_symbolic_tracking_ref_cancellable(repository, upstream, cancelled, child_slot)?
        .is_none()
    {
        return Ok(None);
    }
    let Some(attempt) = publish_ref_cancellable(
        repository,
        &upstream.tracking_ref,
        &revision,
        before.as_deref(),
        cancelled,
        child_slot,
    )?
    else {
        return Ok(None);
    };
    if let Err(error) = attempt {
        // The same tolerance the blocking path has, over the same
        // single-process read: a ref already holding exactly what this
        // fetch obtained was published by somebody who got there first, and
        // is not the rollback the guard exists to stop. A symbolic ref
        // reports no object of its own and so is never accepted here.
        let Some(current) = direct_ref_object_cancellable(
            repository,
            &upstream.tracking_ref,
            cancelled,
            child_slot,
        )?
        else {
            return Ok(None);
        };
        if current.as_deref() != Some(revision.as_str()) {
            return Err(error);
        }
    }
    Ok(Some(revision))
}

fn fetch_reported_revision_cancellable(
    repository: &Path,
    upstream: &Upstream,
    transport: FetchTransport,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<String>> {
    let destination = fetch_destination();
    let op = fetch_op(upstream, transport, &destination);
    let arguments = op.arguments();
    let child = command(repository, &op)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(Error::GitUnavailable)?;
    let Some(output) =
        collect_cancellable_child(child, cancelled, child_slot, Some(MAX_FETCH_OUTPUT_BYTES))?
    else {
        return Ok(None);
    };
    if !output.status.success() {
        return Err(git_error(repository, &arguments, &output));
    }
    let Some(revision) = reported_revision(&output.stdout, &destination) else {
        return Err(Error::FetchUnreported);
    };
    // The same evidence check as the blocking path: the report is the
    // transport's word, and the object store is what the user can be shown.
    let Some(present) = optional_cancellable(
        repository,
        UpdateOp::CatFile(revision.clone()),
        cancelled,
        child_slot,
    )?
    else {
        return Ok(None);
    };
    if present.is_none() {
        return Err(Error::FetchedCommitMissing { revision });
    }
    Ok(Some(revision))
}

fn run_bounded(repository: &Path, op: &UpdateOp, limit: usize) -> Result<Output> {
    let mut child = command(repository, op)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(Error::GitUnavailable)?;
    let mut stdout = child.stdout.take().ok_or(Error::InvalidGitOutput)?;
    let mut stderr = child.stderr.take().ok_or(Error::InvalidGitOutput)?;
    let stdout_reader = std::thread::spawn(move || read_bounded(&mut stdout, limit));
    let stderr_reader = std::thread::spawn(move || read_bounded(&mut stderr, limit));
    let status = child.wait().map_err(Error::GitUnavailable)?;
    let stdout = stdout_reader
        .join()
        .map_err(|_| io::Error::other("Git stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| io::Error::other("Git stderr reader panicked"))??;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn read_bounded(reader: &mut impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut retained = Vec::new();
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            return Ok(retained);
        }
        let remaining = limit.saturating_sub(retained.len());
        retained.extend_from_slice(&chunk[..read.min(remaining)]);
    }
}

fn upstream_refspec(upstream: &Upstream, destination: &str) -> String {
    format!("+{}:{}", upstream.merge_ref, destination)
}

pub fn merge_base(repository: &Path, left: &str, right: &str) -> Result<Option<String>> {
    let op = UpdateOp::MergeBase(left.into(), right.into());
    let arguments = op.arguments();
    let output = run(repository, op)?;
    if output.status.success() {
        return Ok(Some(text(output.stdout)));
    }
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    Err(git_error(repository, &arguments, &output))
}

pub(crate) fn merge_base_cancellable(
    repository: &Path,
    left: &str,
    right: &str,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<Option<String>>> {
    let op = UpdateOp::MergeBase(left.into(), right.into());
    let arguments = op.arguments();
    let Some(output) = run_cancellable(repository, &op, cancelled, child_slot)? else {
        return Ok(None);
    };
    if output.status.success() {
        return Ok(Some(Some(text(output.stdout))));
    }
    if output.status.code() == Some(1) {
        return Ok(Some(None));
    }
    Err(git_error(repository, &arguments, &output))
}

pub fn ahead_behind(repository: &Path, left: &str, right: &str) -> Result<AheadBehind> {
    let value = text(required(
        repository,
        UpdateOp::AheadBehind(left.into(), right.into()),
    )?);
    let mut fields = value.split_whitespace();
    let ahead = fields
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or(Error::InvalidGitOutput)?;
    let behind = fields
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or(Error::InvalidGitOutput)?;
    if fields.next().is_some() {
        return Err(Error::InvalidGitOutput);
    }
    Ok(AheadBehind { ahead, behind })
}

pub(crate) fn ahead_behind_cancellable(
    repository: &Path,
    left: &str,
    right: &str,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<AheadBehind>> {
    let Some(value) = required_cancellable(
        repository,
        UpdateOp::AheadBehind(left.into(), right.into()),
        cancelled,
        child_slot,
    )?
    else {
        return Ok(None);
    };
    parse_ahead_behind(&text(value)).map(Some)
}

fn parse_ahead_behind(value: &str) -> Result<AheadBehind> {
    let mut fields = value.split_whitespace();
    let ahead = fields
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or(Error::InvalidGitOutput)?;
    let behind = fields
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or(Error::InvalidGitOutput)?;
    if fields.next().is_some() {
        return Err(Error::InvalidGitOutput);
    }
    Ok(AheadBehind { ahead, behind })
}

pub fn changed_paths(repository: &Path, left: &str, right: &str) -> Result<Vec<ChangedPath>> {
    let bytes = required(
        repository,
        UpdateOp::ChangedPaths(left.into(), right.into()),
    )?;
    parse_changed_paths(bytes)
}

pub(crate) fn changed_paths_cancellable(
    repository: &Path,
    left: &str,
    right: &str,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<Vec<ChangedPath>>> {
    let Some(bytes) = required_cancellable(
        repository,
        UpdateOp::ChangedPaths(left.into(), right.into()),
        cancelled,
        child_slot,
    )?
    else {
        return Ok(None);
    };
    parse_changed_paths(bytes).map(Some)
}

fn parse_changed_paths(bytes: Vec<u8>) -> Result<Vec<ChangedPath>> {
    let mut fields = bytes
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    let mut paths = Vec::new();
    while let Some(header) = fields.next() {
        let header = header.strip_prefix(b":").ok_or(Error::InvalidGitOutput)?;
        let mut header_fields = header.split(|byte| byte.is_ascii_whitespace());
        let old_mode = header_fields.next().ok_or(Error::InvalidGitOutput)?;
        let new_mode = header_fields.next().ok_or(Error::InvalidGitOutput)?;
        let _old_object = header_fields.next().ok_or(Error::InvalidGitOutput)?;
        let _new_object = header_fields.next().ok_or(Error::InvalidGitOutput)?;
        let status = header_fields.next().ok_or(Error::InvalidGitOutput)?;
        if header_fields.next().is_some() {
            return Err(Error::InvalidGitOutput);
        }
        let first_path = fields.next().ok_or(Error::InvalidGitOutput)?;
        let kind = match status.first().copied() {
            Some(b'A') => ChangeKind::Added,
            Some(b'C') => ChangeKind::Added,
            Some(b'M') => ChangeKind::Modified,
            Some(b'D') => ChangeKind::Deleted,
            Some(b'T') => ChangeKind::TypeChanged,
            _ => ChangeKind::Other,
        };
        let (path, renamed_from) = if matches!(status.first(), Some(b'R' | b'C')) {
            let second_path = fields.next().ok_or(Error::InvalidGitOutput)?;
            (
                path_from_bytes(second_path.to_vec())?,
                matches!(status.first(), Some(b'R'))
                    .then(|| path_from_bytes(first_path.to_vec()))
                    .transpose()?,
            )
        } else {
            (path_from_bytes(first_path.to_vec())?, None)
        };
        let gitlink = old_mode == b"160000" || new_mode == b"160000";
        paths.push(ChangedPath {
            kind,
            path,
            gitlink,
            renamed_from,
        });
    }
    Ok(paths)
}

pub fn commit_summaries(repository: &Path, left: &str, right: &str) -> Result<Vec<String>> {
    let bytes = required(
        repository,
        UpdateOp::CommitSummaries(left.into(), right.into()),
    )?;
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .map(str::to_owned)
        .collect())
}

pub fn commit_exists(repository: &Path, revision: &str) -> Result<bool> {
    Ok(optional(repository, UpdateOp::CatFile(revision.into()))?.is_some())
}

/// The mode of the entry `path` names in the exact commit tree, if it names
/// one. Literal pathspec handling keeps catalog names from becoming Git
/// pattern syntax.
fn tree_entry_mode(repository: &Path, revision: &str, path: &Path) -> Result<Option<Vec<u8>>> {
    let bytes = required(
        repository,
        UpdateOp::TreeEntryMode {
            revision: revision.into(),
            path: path.into(),
        },
    )?;
    // `<mode> SP <type> SP <object> TAB <name>`, NUL-terminated per record.
    Ok(bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .find_map(|record| {
            record
                .split(|byte| *byte == b'\t')
                .next()
                .and_then(|attributes| attributes.split(|byte| *byte == b' ').next())
                .map(<[u8]>::to_vec)
        }))
}

/// Whether `path` names a regular file — not a directory, and not a symbolic
/// link — in the exact commit tree.
///
/// Asked of a candidate's `SKILL.md`, this is what decides whether a revision
/// still holds a skill there. Directory existence cannot answer it: upstream
/// can delete a candidate's skill document while leaving another tracked file
/// beside it, and a catalog whose skill is the repository root has a directory
/// at every revision no matter what the root contains.
///
/// The object type cannot answer it either. Git records a symbolic link as a
/// blob, and both the source scanner and portable validation require the skill
/// document to be a regular file that is not a link, so a `SKILL.md` replaced
/// by a symbolic link would read as a retained skill here while the scan that
/// follows the write reports the installation as no longer loadable. The mode
/// is what distinguishes them: `100644` and `100755` are regular files, and
/// `120000` is a link.
pub fn tree_regular_file_exists(repository: &Path, revision: &str, path: &Path) -> Result<bool> {
    Ok(tree_entry_mode(repository, revision, path)?.is_some_and(|mode| mode.starts_with(b"100")))
}

/// Whether the exact commit tree holds anything at all at `path`.
///
/// A directory, a regular file, a symbolic link, or a submodule: what matters
/// where a removal was disclosed is only whether an installed link would still
/// have something to resolve to.
pub fn tree_entry_exists(repository: &Path, revision: &str, path: &Path) -> Result<bool> {
    Ok(tree_entry_mode(repository, revision, path)?.is_some())
}

/// Whether `path` names a directory in the exact commit tree, if it names
/// anything.
///
/// `None` is "nothing there", which is not the same as "there but not a
/// directory": an ancestor the update turns into a symbolic link redirects
/// every path under it, while an ancestor the update deletes outright leaves
/// nothing to redirect.
pub fn tree_directory_entry(
    repository: &Path,
    revision: &str,
    path: &Path,
) -> Result<Option<bool>> {
    Ok(tree_entry_mode(repository, revision, path)?.map(|mode| {
        // Git prints a directory as `040000`; the rest of the modes it records
        // are files (`100644`, `100755`), symbolic links (`120000`), and
        // submodules (`160000`).
        let mode = mode
            .iter()
            .position(|byte| *byte != b'0')
            .map_or(&[][..], |start| &mode[start..])
            .to_vec();
        mode == b"40000"
    }))
}

pub fn worktree_state(repository: &Path) -> Result<WorktreeState> {
    let filter_settings = configured_filter_settings(repository)?;
    let configured_drivers = filter_settings
        .iter()
        .filter_map(|(key, _)| configured_filter_driver(key))
        .collect::<HashSet<_>>();
    let op = UpdateOp::Status;
    let arguments = op.arguments();
    let mut command = command(repository, &op);
    command.env("GIT_CONFIG_COUNT", filter_settings.len().to_string());
    for (offset, (key, value)) in filter_settings.iter().enumerate() {
        let index = offset;
        command
            .env(format!("GIT_CONFIG_KEY_{index}"), key)
            .env(format!("GIT_CONFIG_VALUE_{index}"), value);
    }
    let output = command.output().map_err(Error::GitUnavailable)?;
    if !output.status.success() {
        return Err(git_error(repository, &arguments, &output));
    }
    parse_worktree_status(output.stdout, &configured_drivers, Some(repository))
}

fn parse_worktree_status(
    bytes: Vec<u8>,
    configured_drivers: &HashSet<&str>,
    filter_repository: Option<&Path>,
) -> Result<WorktreeState> {
    let mut result = WorktreeState::default();
    let mut filter_ambiguous_paths = Vec::new();
    let mut filter_ambiguous_path_bytes = 0_usize;
    let mut records = bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty());
    while let Some(record) = records.next() {
        if record.len() < 3 {
            return Err(Error::InvalidGitOutput);
        }
        if &record[..2] == b"??" {
            result
                .untracked
                .push(path_from_bytes(record[3..].to_vec())?);
        } else if &record[..2] == b"!!" {
            result.ignored.push(path_from_bytes(record[3..].to_vec())?);
        } else {
            let has_second_path =
                matches!(record[0], b'R' | b'C') || matches!(record[1], b'R' | b'C');
            result.index_dirty |= record[0] != b' ';
            if record[1] != b' ' {
                if record[..3] == *b" M " && !configured_drivers.is_empty() {
                    let path = &record[3..];
                    let Some(next_bytes) = filter_ambiguous_path_bytes.checked_add(path.len())
                    else {
                        result.worktree_dirty_known = false;
                        continue;
                    };
                    if filter_ambiguous_paths.len() == MAX_FILTERED_STATUS_PATHS
                        || next_bytes > MAX_FILTERED_STATUS_PATH_BYTES
                    {
                        result.worktree_dirty_known = false;
                        continue;
                    }
                    filter_ambiguous_path_bytes = next_bytes;
                    filter_ambiguous_paths.push(path.to_vec());
                } else {
                    result.worktree_dirty = true;
                }
            }
            if has_second_path && records.next().is_none() {
                return Err(Error::InvalidGitOutput);
            }
        }
    }
    if !filter_ambiguous_paths.is_empty() && result.worktree_dirty_known {
        if let Some(repository) = filter_repository {
            let drivers = filter_drivers_for_paths(repository, &filter_ambiguous_paths)?;
            if drivers.iter().all(|driver| {
                driver
                    .as_deref()
                    .is_some_and(|driver| configured_drivers.contains(driver))
            }) {
                result.worktree_dirty_known = false;
            } else {
                result.worktree_dirty = true;
            }
        } else {
            // A cancellable update check does not launch a second check-attr
            // protocol whose stdin writer could outlive cancellation. The
            // conservative answer is the same withheld cleanliness verdict
            // used when a configured filter prevents a safe comparison.
            result.worktree_dirty_known = false;
        }
    }
    Ok(result)
}

pub(crate) fn worktree_state_cancellable(
    repository: &Path,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<WorktreeState>> {
    let op = UpdateOp::FilterSettings;
    let arguments = op.arguments();
    let Some(output) = run_cancellable(repository, &op, cancelled, child_slot)? else {
        return Ok(None);
    };
    let filter_settings = if !output.status.success() {
        if output.status.code() == Some(1) && output.stdout.is_empty() {
            Vec::new()
        } else {
            return Err(git_error(repository, &arguments, &output));
        }
    } else {
        parse_filter_settings(output.stdout)?
    };
    let configured_drivers = filter_settings
        .iter()
        .filter_map(|(key, _)| configured_filter_driver(key))
        .collect::<HashSet<_>>();
    let op = UpdateOp::Status;
    let arguments = op.arguments();
    let mut status_command = command(repository, &op);
    status_command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_CONFIG_COUNT", filter_settings.len().to_string());
    for (offset, (key, value)) in filter_settings.iter().enumerate() {
        status_command
            .env(format!("GIT_CONFIG_KEY_{offset}"), key)
            .env(format!("GIT_CONFIG_VALUE_{offset}"), value);
    }
    let child = status_command.spawn().map_err(Error::GitUnavailable)?;
    let Some(output) = collect_cancellable_child(child, cancelled, child_slot, None)? else {
        return Ok(None);
    };
    if !output.status.success() {
        return Err(git_error(repository, &arguments, &output));
    }
    parse_worktree_status(output.stdout, &configured_drivers, None).map(Some)
}

fn configured_filter_settings(repository: &Path) -> Result<Vec<(String, &'static str)>> {
    let op = UpdateOp::FilterSettings;
    let arguments = op.arguments();
    let output = run(repository, op)?;
    if !output.status.success() {
        if output.status.code() == Some(1) && output.stdout.is_empty() {
            return Ok(Vec::new());
        }
        return Err(git_error(repository, &arguments, &output));
    }
    parse_filter_settings(output.stdout)
}

fn parse_filter_settings(bytes: Vec<u8>) -> Result<Vec<(String, &'static str)>> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|key| !key.is_empty())
        .map(|key| {
            let key = String::from_utf8(key.to_vec()).map_err(|_| Error::InvalidGitOutput)?;
            let value = if key
                .rsplit_once('.')
                .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case("required"))
            {
                "false"
            } else {
                ""
            };
            Ok((key, value))
        })
        .collect()
}

fn configured_filter_driver(key: &str) -> Option<&str> {
    let (driver, setting) = key.strip_prefix("filter.")?.rsplit_once('.')?;
    matches!(setting, "clean" | "process").then_some(driver)
}

fn filter_drivers_for_paths(repository: &Path, paths: &[Vec<u8>]) -> Result<Vec<Option<String>>> {
    let op = UpdateOp::CheckAttr;
    let arguments = op.arguments();
    let mut child = command(repository, &op)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(Error::GitUnavailable)?;
    let mut input = Vec::new();
    for path in paths {
        input.extend_from_slice(path);
        input.push(0);
    }
    let mut stdin = child.stdin.take().expect("Git check-attr stdin is piped");
    let mut stdout = child.stdout.take().expect("Git check-attr stdout is piped");
    let mut stderr = child.stderr.take().expect("Git check-attr stderr is piped");
    let writer = std::thread::spawn(move || -> io::Result<()> {
        stdin.write_all(&input)?;
        drop(stdin);
        Ok(())
    });
    let stdout_reader = std::thread::spawn(move || -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes)?;
        Ok(bytes)
    });
    let stderr_reader = std::thread::spawn(move || -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes)?;
        Ok(bytes)
    });
    let status = child.wait().map_err(Error::GitUnavailable)?;
    writer
        .join()
        .map_err(|_| io::Error::other("Git check-attr input writer panicked"))??;
    let stdout = stdout_reader
        .join()
        .map_err(|_| io::Error::other("Git check-attr stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| io::Error::other("Git check-attr stderr reader panicked"))??;
    if !status.success() {
        return Err(git_error(
            repository,
            &arguments,
            &Output {
                status,
                stdout,
                stderr,
            },
        ));
    }
    let mut fields = stdout.split(|byte| *byte == 0);
    let mut drivers = Vec::with_capacity(paths.len());
    for expected_path in paths {
        let (Some(path), Some(attribute), Some(value)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return Err(Error::InvalidGitOutput);
        };
        if path != expected_path || attribute != b"filter" {
            return Err(Error::InvalidGitOutput);
        }
        let value = String::from_utf8(value.to_vec()).map_err(|_| Error::InvalidGitOutput)?;
        drivers.push((!matches!(value.as_str(), "unspecified" | "unset")).then_some(value));
    }
    if fields.any(|field| !field.is_empty()) {
        return Err(Error::InvalidGitOutput);
    }
    Ok(drivers)
}

/// Fast-forward to the exact object shown in the preview. Git's own hooks and
/// checkout filters run; inspection-only configuration must not leak here.
pub fn fast_forward(repository: &Path, revision: &str) -> Result<()> {
    let op = UpdateOp::Merge(revision.into());
    let arguments = op.arguments();
    let output = run(repository, op)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(git_error(repository, &arguments, &output))
    }
}

pub fn update_operation_fixtures() -> Vec<OperationFixture> {
    let ops = vec![
        UpdateOp::RevParse("HEAD".into()),
        UpdateOp::AbsoluteGitDir,
        UpdateOp::SymbolicHead,
        UpdateOp::SymbolicRef("refs/remotes/origin/main".into()),
        UpdateOp::UpstreamRef("refs/heads/main".into()),
        UpdateOp::ConfigGet("core.sshCommand".into()),
        UpdateOp::UserSshCommand,
        UpdateOp::TransportSettings,
        UpdateOp::TransportPolicy,
        UpdateOp::FilterSettings,
        UpdateOp::PromisorSettings,
        UpdateOp::CheckAttr,
        UpdateOp::MergeBase("a".into(), "b".into()),
        UpdateOp::AheadBehind("a".into(), "b".into()),
        UpdateOp::ChangedPaths("a".into(), "b".into()),
        UpdateOp::CommitSummaries("a".into(), "b".into()),
        UpdateOp::CatFile("abc".into()),
        UpdateOp::TreeEntryMode {
            revision: "abc".into(),
            path: "skills/demo/SKILL.md".into(),
        },
        UpdateOp::Status,
        UpdateOp::Fetch {
            remote: "origin".into(),
            refspec: upstream_refspec(
                &Upstream {
                    branch: "main".into(),
                    remote: "origin".into(),
                    merge_ref: "refs/heads/main".into(),
                    tracking_ref: "refs/remotes/origin/main".into(),
                    revision: None,
                },
                &fetch_destination(),
            ),
            ssh_command: "ssh -o BatchMode=yes".into(),
            // What an unrestricted environment and an unset policy narrow to,
            // which is the ceiling itself.
            allowed_protocols: narrowed_transports(
                &TransportPolicy::default(),
                &TransportBoundary {
                    allowed: None,
                    from_user: true,
                },
            ),
            hooks_path: SUPPRESSED_HOOKS_PATH.into(),
        },
        UpdateOp::PublishRef {
            reference: "refs/remotes/origin/main".into(),
            revision: "abc".into(),
            expected: ABSENT_REF.into(),
            hooks_path: SUPPRESSED_HOOKS_PATH.into(),
        },
        UpdateOp::Merge("abc".into()),
    ];
    ops.into_iter()
        .map(|op| OperationFixture {
            subcommand: op.subcommand(),
            arguments: op.arguments(),
            environment: op.environment(),
        })
        .collect()
}

pub fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes)
        .trim_end_matches(['\r', '\n'])
        .to_owned()
}

fn git_error(repository: &Path, arguments: &[OsString], output: &Output) -> Error {
    Error::GitCommand {
        repository: repository.to_path_buf(),
        arguments: arguments
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect(),
        // Git diagnostics are not a safe persistence boundary: transports and
        // credential helpers may echo a credential-bearing remote URL. Update
        // failures are cached and later rendered, so retain only the process
        // status here rather than allowing arbitrary stderr into metadata.
        stderr: output.status.code().map_or_else(
            || "Git command terminated without an exit status".to_owned(),
            |code| {
                format!(
                    "Git command exited with status {code}: {}",
                    classified_diagnostic(&output.stderr)
                )
            },
        ),
    }
}

fn classified_diagnostic(stderr: &[u8]) -> &'static str {
    let diagnostic = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    if diagnostic.contains("authentication failed")
        || diagnostic.contains("could not read username")
        || diagnostic.contains("permission denied")
        || diagnostic.contains("publickey")
    {
        "authentication failed"
    } else if diagnostic.contains("could not resolve host") {
        "the remote host could not be resolved"
    } else if diagnostic.contains("failed to connect")
        || diagnostic.contains("couldn't connect")
        || diagnostic.contains("connection timed out")
        || diagnostic.contains("connection refused")
    {
        "the remote host could not be reached"
    } else if diagnostic.contains("certificate") || diagnostic.contains("ssl") {
        "the remote TLS certificate was rejected"
    } else if diagnostic.contains("couldn't find remote ref") {
        "the configured remote reference was not found"
    } else if diagnostic.contains("not a git repository") {
        "the repository could not be opened"
    } else if diagnostic.contains("unknown option") || diagnostic.contains("unsupported option") {
        "the installed Git does not support a required option"
    } else {
        "the diagnostic was omitted because it may contain credentials"
    }
}

#[cfg(unix)]
fn path_from_bytes(bytes: Vec<u8>) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: Vec<u8>) -> Result<PathBuf> {
    String::from_utf8(bytes)
        .map(PathBuf::from)
        .map_err(|_| Error::InvalidGitOutput)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;

    /// Scope is the whole distinction the refusal rests on: the same key is
    /// the user's own configuration in one scope and a refusal in another.
    #[test]
    fn only_repository_scoped_transport_settings_are_reported() {
        let record = |pairs: &[(&str, &str)]| {
            let mut bytes = Vec::new();
            for (scope, name) in pairs {
                bytes.extend_from_slice(scope.as_bytes());
                bytes.push(0);
                bytes.extend_from_slice(name.as_bytes());
                bytes.push(b'\n');
                bytes.extend_from_slice(b"/opt/program.sh");
                bytes.push(0);
            }
            bytes
        };

        // The user's own configuration keeps working.
        assert_eq!(
            parse_transport_settings(record(&[
                ("global", "credential.helper"),
                ("system", "core.sshcommand"),
                ("unknown", "credential.helper"),
            ])),
            None
        );
        // The checkout's own does not.
        assert_eq!(
            parse_transport_settings(record(&[
                ("global", "credential.helper"),
                ("local", "remote.origin.uploadpack"),
            ])),
            Some("remote.origin.uploadpack".to_owned())
        );
        // Worktree-scoped configuration is the repository's too.
        assert_eq!(
            parse_transport_settings(record(&[("worktree", "core.sshcommand")])),
            Some("core.sshcommand".to_owned())
        );
        assert_eq!(parse_transport_settings(Vec::new()), None);
    }

    /// An allowlist, so a transport Git does not implement itself is refused
    /// whether or not anyone thought of it here.
    #[test]
    fn helper_transports_are_recognised_by_url() {
        assert!(remote_url_runs_a_helper("ext::sh -c whoami"));
        assert!(remote_url_runs_a_helper("fd::7,8"));
        assert!(remote_url_runs_a_helper("  ext::sh -c whoami"));
        // Any `<transport>::` form runs `git-remote-<transport>`, not only the
        // two spellings that are obviously dangerous.
        assert!(remote_url_runs_a_helper("hg::https://example.test/repo"));
        assert!(remote_url_runs_a_helper("anything::somewhere"));
        // An unrecognised scheme runs a helper too.
        assert!(remote_url_runs_a_helper("xyz://example.test/repo.git"));
        // Including one that is only unrecognised by its capitals. A URL
        // scheme is case-insensitive to read, but Git is not one of its
        // readers: it compares the spelling literally, so `FILE://` sends it
        // looking for `git-remote-FILE` and it says so — "'remote-FILE' is not
        // a git command". Treating that as the built-in transport would let
        // the preflight pass a helper through, and the fetch's own allowlist
        // names the lower-case spellings only.
        assert!(remote_url_runs_a_helper("HTTPS://example.test/repo.git"));
        assert!(remote_url_runs_a_helper("FILE:///var/tmp/checkout"));
        assert!(remote_url_runs_a_helper("Ssh://git@example.test/repo.git"));

        for handled in [
            "https://example.test/repo.git",
            "http://example.test/repo.git",
            "ssh://git@example.test/repo.git",
            "git://example.test/repo.git",
            "file:///var/tmp/checkout",
            // Deprecated, but Git fetches these itself.
            "ftp://example.test/repo.git",
            "ftps://example.test/repo.git",
        ] {
            assert!(!remote_url_runs_a_helper(handled), "{handled}");
        }
        assert!(!remote_url_runs_a_helper("/var/tmp/checkout"));
        assert!(!remote_url_runs_a_helper("../sibling"));
        assert!(!remote_url_runs_a_helper("git@example.test:repo.git"));
        // A path that merely contains the separator is not a transport.
        assert!(!remote_url_runs_a_helper("/var/tmp/ext::repo"));
    }

    /// A repository that explicitly turns a helper off names no program, and
    /// refusing it would block a check for no gain.
    #[test]
    fn transport_settings_disabled_by_value_are_not_refused() {
        let record = |pairs: &[(&str, &str, &str)]| {
            let mut bytes = Vec::new();
            for (scope, name, value) in pairs {
                bytes.extend_from_slice(scope.as_bytes());
                bytes.push(0);
                bytes.extend_from_slice(name.as_bytes());
                if !value.is_empty() {
                    bytes.push(b'\n');
                    bytes.extend_from_slice(value.as_bytes());
                }
                bytes.push(0);
            }
            bytes
        };

        assert_eq!(
            parse_transport_settings(record(&[
                ("local", "credential.helper", ""),
                ("local", "core.gitproxy", "none"),
                ("local", "core.askpass", ""),
            ])),
            None
        );
        assert_eq!(
            parse_transport_settings(record(&[
                ("local", "credential.helper", ""),
                ("local", "core.sshcommand", "/opt/ssh.sh"),
            ])),
            Some("core.sshcommand".to_owned())
        );
        assert_eq!(
            parse_transport_settings(record(&[("local", "core.gitproxy", "/opt/proxy.sh")])),
            Some("core.gitproxy".to_owned())
        );
    }

    /// A later record undoes an earlier one, so the decision is about the
    /// value that survives rather than the first one that looks like a
    /// command.
    #[test]
    fn transport_settings_are_judged_after_resets_and_overrides() {
        let record = |pairs: &[(&str, &str, &str)]| {
            let mut bytes = Vec::new();
            for (scope, name, value) in pairs {
                bytes.extend_from_slice(scope.as_bytes());
                bytes.push(0);
                bytes.extend_from_slice(name.as_bytes());
                bytes.push(b'\n');
                bytes.extend_from_slice(value.as_bytes());
                bytes.push(0);
            }
            bytes
        };

        // An empty credential helper resets the list, so nothing is left to
        // run and the repository is not refused.
        assert_eq!(
            parse_transport_settings(record(&[
                ("local", "credential.helper", "/opt/helper.sh"),
                ("local", "credential.helper", ""),
            ])),
            None
        );
        // A helper added back after the reset is live again.
        assert_eq!(
            parse_transport_settings(record(&[
                ("local", "credential.helper", "/opt/helper.sh"),
                ("local", "credential.helper", ""),
                ("local", "credential.helper", "/opt/later.sh"),
            ])),
            Some("credential.helper".to_owned())
        );
        // A scalar's last value wins, so one emptied afterwards runs nothing.
        assert_eq!(
            parse_transport_settings(record(&[
                ("local", "core.sshcommand", "/opt/ssh.sh"),
                ("worktree", "core.sshcommand", ""),
            ])),
            None
        );
        // And a user value the checkout overrides is the checkout's problem.
        assert_eq!(
            parse_transport_settings(record(&[
                ("global", "core.sshcommand", "/home/user/ssh.sh"),
                ("local", "core.sshcommand", "/opt/ssh.sh"),
            ])),
            Some("core.sshcommand".to_owned())
        );
        // Worktree configuration is the repository's own, so overriding one
        // repository value with another is still a refusal.
        assert_eq!(
            parse_transport_settings(record(&[
                ("local", "core.sshcommand", "/opt/ssh.sh"),
                ("worktree", "core.sshcommand", "/opt/other.sh"),
            ])),
            Some("core.sshcommand".to_owned())
        );
        // The reset survives a live helper from the user's own configuration.
        assert_eq!(
            parse_transport_settings(record(&[
                ("global", "credential.helper", "osxkeychain"),
                ("local", "credential.helper", ""),
            ])),
            None
        );
    }

    /// `core.gitProxy` is neither a list nor a scalar: Git takes the first
    /// entry whose optional `for DOMAIN` suffix matches, so a proxy command
    /// aimed at the upstream host stays in force behind a later blanket
    /// `none`. Reading only the last value would call that disabled.
    #[test]
    fn a_domain_scoped_proxy_command_is_not_disabled_by_a_later_none() {
        let record = |pairs: &[(&str, &str, &str)]| {
            let mut bytes = Vec::new();
            for (scope, name, value) in pairs {
                bytes.extend_from_slice(scope.as_bytes());
                bytes.push(0);
                bytes.extend_from_slice(name.as_bytes());
                bytes.push(b'\n');
                bytes.extend_from_slice(value.as_bytes());
                bytes.push(0);
            }
            bytes
        };

        assert_eq!(
            parse_transport_settings(record(&[
                ("local", "core.gitproxy", "/opt/proxy.sh for example.test"),
                ("local", "core.gitproxy", "none"),
            ])),
            Some("core.gitproxy".to_owned())
        );
        // A repository that only ever disables it still checks.
        assert_eq!(
            parse_transport_settings(record(&[("local", "core.gitproxy", "none")])),
            None
        );
        // The user's own proxy is theirs, however many entries it has.
        assert_eq!(
            parse_transport_settings(record(&[
                (
                    "global",
                    "core.gitproxy",
                    "/home/user/proxy.sh for example.test"
                ),
                ("global", "core.gitproxy", "none"),
            ])),
            None
        );
    }

    /// The publication is a compare-and-swap, and these are the two answers
    /// that are not a plain success: the ref moved to something else, which
    /// must fail rather than roll the user's newer value back, and the ref
    /// already holds exactly what was reported, which is nothing to refuse.
    #[cfg(unix)]
    #[test]
    fn publication_refuses_a_moved_tracking_ref_but_accepts_an_identical_one() {
        let Ok(temporary) = tempfile::tempdir() else {
            return;
        };
        let repository = temporary.path().join("repo");
        let run_git = |arguments: &[&str]| -> Option<String> {
            let output = Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args(arguments)
                .output()
                .ok()?;
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        };
        if std::fs::create_dir_all(&repository).is_err()
            || Command::new("git")
                .args(["init", "-q", "-b", "main"])
                .arg(&repository)
                .output()
                .is_err()
        {
            return;
        }
        if std::fs::write(repository.join("a.txt"), "one\n").is_err() {
            return;
        }
        let Some(()) = run_git(&["add", "."]).map(|_| ()) else {
            return;
        };
        let committed = run_git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-qm",
            "one",
        ]);
        if committed.is_none() {
            return;
        }
        let Some(first) = run_git(&["rev-parse", "HEAD"]) else {
            return;
        };
        if std::fs::write(repository.join("a.txt"), "two\n").is_err() {
            return;
        }
        run_git(&["add", "."]);
        run_git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-qm",
            "two",
        ]);
        let Some(second) = run_git(&["rev-parse", "HEAD"]) else {
            return;
        };

        let upstream = Upstream {
            remote: "origin".into(),
            merge_ref: "refs/heads/main".into(),
            tracking_ref: "refs/remotes/origin/main".into(),
            branch: "main".into(),
            revision: Some(first.clone()),
        };
        run_git(&[
            "update-ref",
            "--no-deref",
            &upstream.tracking_ref,
            &second,
            ABSENT_REF,
        ]);

        // The ref now holds `second` while the check obtained `first` and
        // expected to be replacing nothing: publishing would roll it back.
        assert!(
            publish_or_accept_identical(&repository, &upstream, &first, None).is_err(),
            "publication rolled a moved tracking ref back"
        );
        assert_eq!(
            run_git(&["rev-parse", &upstream.tracking_ref]).as_deref(),
            Some(second.as_str())
        );

        // The ref already holds exactly what was reported, which is not the
        // rollback the compare-and-swap exists to stop.
        assert!(
            publish_or_accept_identical(&repository, &upstream, &second, None).is_ok(),
            "publication refused a ref already at the reported object"
        );
    }

    #[test]
    fn every_typed_update_operation_is_allowlisted() {
        for fixture in update_operation_fixtures() {
            assert!(UPDATE_SUBCOMMANDS.contains(&fixture.subcommand));
        }
    }

    #[test]
    fn merge_does_not_inherit_inspection_environment() {
        let merge = update_operation_fixtures()
            .into_iter()
            .find(|f| f.subcommand == "merge")
            .unwrap();
        assert!(merge.environment.is_empty());
        assert!(
            merge
                .arguments
                .iter()
                .any(|arg| arg == OsStr::new("--no-autostash"))
        );
        assert!(
            merge
                .arguments
                .windows(2)
                .any(|args| args == [OsStr::new("-c"), OsStr::new("maintenance.auto=false")])
        );
        assert!(
            merge
                .arguments
                .iter()
                .any(|arg| arg == OsStr::new("--no-squash"))
        );
        assert!(
            !merge
                .arguments
                .iter()
                .any(|arg| arg == OsStr::new("core.untrackedCache=false"))
        );
    }

    /// `merge.verifySignatures` is the user's policy wherever they set it, and
    /// `--no-verify-signatures` would overrule it — fast-forwarding to an
    /// unsigned tip Git had been told to refuse. The program it can pull in is
    /// disclosed in the plan instead, which is how this operation answers for
    /// everything else it may run.
    #[test]
    fn merge_does_not_overrule_a_configured_signature_policy() {
        let merge = update_operation_fixtures()
            .into_iter()
            .find(|fixture| fixture.subcommand == "merge")
            .expect("merge fixture");
        assert!(
            !merge
                .arguments
                .iter()
                .any(|argument| argument == OsStr::new("--no-verify-signatures"))
        );
        assert!(
            !merge
                .arguments
                .iter()
                .any(|argument| argument == OsStr::new("--verify-signatures"))
        );
    }

    /// Git dereferences a symbolic ref when a transaction updates one, so a
    /// fetch that names the remote-tracking ref writes wherever that name
    /// points. Checking the name first cannot close that, because the check and
    /// the fetch are separate processes; the destination has to be somewhere
    /// nothing else could have prepared.
    #[test]
    fn the_fetch_never_names_the_remote_tracking_ref_as_its_destination() {
        let fetch = update_operation_fixtures()
            .into_iter()
            .find(|fixture| fixture.subcommand == "fetch")
            .expect("fetch fixture");
        let refspec = fetch
            .arguments
            .iter()
            .filter_map(|argument| argument.to_str())
            .find(|argument| argument.starts_with('+'))
            .expect("forced refspec");
        let (_, destination) = refspec.split_once(':').expect("refspec destination");

        assert!(
            destination.starts_with("refs/skilled/fetch/"),
            "{destination}"
        );
        assert!(!refspec.contains("refs/remotes/"), "{refspec}");
    }

    /// The transport claim's URL half is enforced by the fetch rather than
    /// carried over from a reading that preceded it. `GIT_ALLOW_PROTOCOL`
    /// overrides `protocol.*` configuration outright, so a helper URL, an
    /// `insteadOf` rewrite, or a permission the checkout grants itself between
    /// the preflight read and this process cannot put a program behind it.
    #[test]
    fn the_fetch_carries_its_own_transport_allowlist() {
        let fetch = update_operation_fixtures()
            .into_iter()
            .find(|fixture| fixture.subcommand == "fetch")
            .expect("fetch fixture");
        let (_, allowed) = fetch
            .environment
            .iter()
            .find(|(key, _)| key == OsStr::new("GIT_ALLOW_PROTOCOL"))
            .expect("transport allowlist");
        let allowed = allowed.to_str().expect("allowlist text");

        // The same answer `remote_url_runs_a_helper` gives, so a transport
        // cannot be refused by one and permitted by the other.
        for protocol in allowed.split(':') {
            assert!(
                !remote_url_runs_a_helper(&format!("{protocol}://host/repository.git")),
                "{protocol} is allowed here but refused as a URL"
            );
        }
        assert!(!allowed.split(':').any(|protocol| protocol == "ext"));
    }

    /// The allowlist overrides every `protocol.*` Git would read, so it may
    /// only ever subtract. A caller's inherited restriction and the user's own
    /// policy each narrow it; the checkout's scopes reach neither, so a
    /// repository can neither grant itself a transport nor take one away.
    #[test]
    fn the_transport_allowlist_only_ever_narrows() {
        let unset = TransportPolicy::default();
        assert_eq!(
            narrowed_transports(&unset, &boundary(None, true)),
            HANDLED_BY_GIT.join(":")
        );

        // An inherited boundary this process has no standing to widen.
        assert_eq!(
            narrowed_transports(&unset, &boundary(Some("https"), true)),
            "https"
        );
        assert_eq!(
            narrowed_transports(&unset, &boundary(Some("https:ext"), true)),
            "https"
        );
        // An inherited allowlist that permits nothing leaves nothing, rather
        // than falling back to the ceiling.
        assert_eq!(narrowed_transports(&unset, &boundary(Some(""), true)), "");
        // Git splits the raw value and matches exactly, so neither of these
        // names a transport it would have permitted either.
        assert_eq!(
            narrowed_transports(&unset, &boundary(Some("HTTPS"), true)),
            ""
        );
        assert_eq!(
            narrowed_transports(&unset, &boundary(Some(" https"), true)),
            ""
        );

        // `protocol.file.allow=never` is a real hardening, and re-enabling it
        // would be the bypass.
        let refused = policy(&record("global", "protocol.file.allow", "never"));
        assert!(!narrowed_transports(&refused, &boundary(None, true)).contains("file"));
        // The blanket default decides the transports no key names, and a named
        // permission outranks it — the order Git resolves them in.
        let excepted = policy(&format!(
            "{}{}",
            record("global", "protocol.allow", "never"),
            record("global", "protocol.https.allow", "always")
        ));
        assert_eq!(
            narrowed_transports(&excepted, &boundary(None, true)),
            "https"
        );
        // A value Git would abort on permits nothing here, rather than being
        // read as consent — including one that is only a recognised spelling
        // once whitespace a quoted value kept is taken off it.
        for unreadable in ["sometimes", "always "] {
            assert_eq!(
                narrowed_transports(
                    &policy(&record("global", "protocol.allow", unreadable)),
                    &boundary(None, true)
                ),
                "",
                "{unreadable:?} was read as permission"
            );
        }
        // The checkout's own policy is nobody's boundary: it neither widens
        // the ceiling nor narrows a fetch on the user's behalf.
        let checkout = policy(&record("local", "protocol.allow", "never"));
        assert_eq!(
            narrowed_transports(&checkout, &boundary(None, true)),
            HANDLED_BY_GIT.join(":")
        );
    }

    /// `user` is not `always`. Git permits a user-only transport exactly when
    /// the operation came from the user, and `file`, `ftp`, and `ftps` sit at
    /// that policy by default — so exporting them unconditionally would turn a
    /// caller's `GIT_PROTOCOL_FROM_USER=0` into permission.
    #[test]
    fn a_user_only_transport_follows_where_the_operation_came_from() {
        let unset = TransportPolicy::default();
        assert_eq!(
            narrowed_transports(&unset, &boundary(None, false)),
            "https:http:ssh:git"
        );
        // Named explicitly, the same policy answers the same way.
        let named = policy(&record("global", "protocol.https.allow", "user"));
        assert!(!narrowed_transports(&named, &boundary(None, false)).contains("https"));
        assert!(narrowed_transports(&named, &boundary(None, true)).contains("https"));
        // A transport the user permitted outright is unaffected by it.
        let always = policy(&record("global", "protocol.file.allow", "always"));
        assert!(narrowed_transports(&always, &boundary(None, false)).contains("file"));
    }

    /// The environment says whether this is a user-initiated operation in
    /// Git's own boolean spelling, and a value Git would abort on cannot be
    /// read here as permission.
    #[cfg(unix)]
    #[test]
    fn the_operations_origin_is_read_the_way_git_reads_a_boolean() {
        use std::os::unix::ffi::OsStringExt;

        let boolean = |value: &str| git_environment_boolean(Some(std::ffi::OsStr::new(value)));

        // Absent is Git's default, and this is the only true answer that
        // widens anything, so it is the one to be sure of.
        assert!(git_environment_boolean(None));
        for truthy in ["true", "YES", "on", "1", "2", "-3"] {
            assert!(boolean(truthy), "{truthy:?}");
        }
        // Every spelling of zero is false, which is the whole reason not to
        // match a short list of words.
        for falsey in ["false", "NO", "off", "", "0", "00", "+0", "-0"] {
            assert!(!boolean(falsey), "{falsey:?}");
        }
        // Git aborts on these; nothing here can, so they narrow instead.
        for unreadable in ["maybe", "yes please"] {
            assert!(!boolean(unreadable), "{unreadable:?}");
        }
        assert!(!git_environment_boolean(Some(
            &std::ffi::OsString::from_vec(b"\xff".to_vec())
        )));
    }

    /// An allowlist this cannot read is still a boundary somebody drew, so it
    /// leaves nothing rather than being treated as absent.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_inherited_allowlist_permits_nothing() {
        use std::os::unix::ffi::OsStringExt;

        let boundary = TransportBoundary {
            allowed: Some(std::ffi::OsString::from_vec(b"https\xff".to_vec())),
            from_user: true,
        };

        assert_eq!(
            narrowed_transports(&TransportPolicy::default(), &boundary),
            ""
        );
    }

    fn boundary(allowed: Option<&str>, from_user: bool) -> TransportBoundary {
        TransportBoundary {
            allowed: allowed.map(std::ffi::OsString::from),
            from_user,
        }
    }

    fn policy(records: &str) -> TransportPolicy {
        parse_transport_policy(records.as_bytes().to_vec())
    }

    fn record(scope: &str, name: &str, value: &str) -> String {
        format!("{scope}\0{name}\n{value}\0")
    }

    /// A `core.sshCommand` inside the checkout is refused before a check runs,
    /// but the refusal and the fetch are separate processes. The value Skilled
    /// exports is therefore chosen by scope at the moment it is read: the
    /// user's own survives, the checkout's is struck out however late it
    /// arrived, and an empty value configures nothing.
    #[test]
    fn only_the_users_own_ssh_command_is_exported() {
        let record = |scope: &str, value: &str| format!("{scope}\0core.sshCommand\n{value}\0");

        assert_eq!(
            parse_user_ssh_command(record("global", "ssh -i key").into_bytes()),
            Some("ssh -i key".to_owned())
        );
        assert_eq!(
            parse_user_ssh_command(record("local", "/checkout/ssh.sh").into_bytes()),
            None
        );
        assert_eq!(
            parse_user_ssh_command(record("worktree", "/checkout/ssh.sh").into_bytes()),
            None
        );
        // A scalar's last value wins, and the checkout's is not eligible to be
        // it however far down the list Git read it.
        assert_eq!(
            parse_user_ssh_command(
                format!(
                    "{}{}",
                    record("global", "ssh -i key"),
                    record("local", "/checkout/ssh.sh")
                )
                .into_bytes()
            ),
            Some("ssh -i key".to_owned())
        );
        assert_eq!(
            parse_user_ssh_command(
                format!(
                    "{}{}",
                    record("system", "ssh -i system"),
                    record("global", "ssh -i user")
                )
                .into_bytes()
            ),
            Some("ssh -i user".to_owned())
        );
        assert_eq!(
            parse_user_ssh_command(record("global", "").into_bytes()),
            None
        );
        assert_eq!(parse_user_ssh_command(Vec::new()), None);
    }

    /// Two invocations must not be able to collide on a destination, or one
    /// could read the other's report line as its own.
    #[test]
    fn every_fetch_destination_is_its_own() {
        assert_ne!(fetch_destination(), fetch_destination());
    }

    /// The report is read from the end of each line because the flag column
    /// may itself be a space, and only an exact, single, fully-spelled object
    /// for the exact destination is believed.
    #[test]
    fn only_an_unambiguous_report_line_yields_a_revision() {
        let destination = "refs/skilled/fetch/a-b-c";
        let object = "6e0ab0cb6e0a6ab7410a1564cbdb4a6b27d45cbd";
        let zeros = "0000000000000000000000000000000000000000";
        let creation = format!("* {zeros} {object} {destination}\n");
        assert_eq!(
            reported_revision(creation.as_bytes(), destination),
            Some(object.to_owned())
        );
        // A space flag: the line a forced update would print.
        let forced = format!("  {object} {object} {destination}\n");
        assert_eq!(
            reported_revision(forced.as_bytes(), destination),
            Some(object.to_owned())
        );
        // Another refspec's line is not this destination's report.
        let other = format!("* {zeros} {object} refs/skilled/fetch/other\n");
        assert_eq!(reported_revision(other.as_bytes(), destination), None);
        // No line at all: a destination this invocation alone chose cannot be
        // up to date, so silence is refused rather than defaulted.
        assert_eq!(reported_revision(b"", destination), None);
        // Two lines for one destination cannot both be the report.
        let doubled = format!("{creation}{creation}");
        assert_eq!(reported_revision(doubled.as_bytes(), destination), None);
        // An all-zero or truncated object is not a fetched revision.
        let absent = format!("- {object} {zeros} {destination}\n");
        assert_eq!(reported_revision(absent.as_bytes(), destination), None);
        let truncated = format!("* {zeros} {} {destination}\n", &object[..12]);
        assert_eq!(reported_revision(truncated.as_bytes(), destination), None);
    }

    /// A symbolic ref holds no object of its own, and the object its referent
    /// holds is not evidence about it. Reporting that object would let a ref
    /// substituted for a symbolic one whose referent happens to hold the
    /// reported revision be accepted as a ref that already holds it.
    // `Output` needs an `ExitStatus`, which only the platform extension traits
    // construct; the parser under test is platform-neutral either way.
    #[cfg(unix)]
    #[test]
    fn only_a_direct_ref_reports_an_object() {
        let object = "1076bd4ad3e2d113b47f5b9ae9cc5da855df5522";
        assert_eq!(
            parse_direct_ref_object(&succeeded(format!("{object}\t"))),
            Some(object.to_owned())
        );
        assert_eq!(
            parse_direct_ref_object(&succeeded(format!("{object}\trefs/heads/victim"))),
            None
        );
        assert_eq!(parse_direct_ref_object(&succeeded(String::new())), None);
    }

    #[cfg(unix)]
    fn succeeded(stdout: String) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: stdout.into_bytes(),
            stderr: Vec::new(),
        }
    }

    /// The tracking ref is written from the reported object rather than
    /// fetched into, and `--no-deref` is what confines losing the substitution
    /// race: the named ref itself is written, never its referent.
    #[test]
    fn ref_updates_never_dereference_and_run_no_repository_hook() {
        let updates = update_operation_fixtures()
            .into_iter()
            .filter(|fixture| fixture.subcommand == "update-ref")
            .collect::<Vec<_>>();
        assert_eq!(updates.len(), 1);
        for update in updates {
            assert!(
                update
                    .arguments
                    .iter()
                    .any(|argument| argument == OsStr::new("--no-deref")),
                "{:?}",
                update.arguments
            );
            assert!(
                update
                    .arguments
                    .iter()
                    .any(|argument| argument.to_string_lossy().starts_with("core.hooksPath=")),
                "{:?}",
                update.arguments
            );
        }
    }

    /// `core.fsmonitor` names a program Git runs, and `core.hooksPath` does not
    /// reach it. A check offered as reading may not execute one; the
    /// fast-forward keeps the repository's own configuration and discloses it.
    #[test]
    fn inspections_suppress_the_filesystem_monitor_and_the_merge_does_not() {
        for fixture in update_operation_fixtures() {
            let suppressed = fixture
                .arguments
                .iter()
                .any(|argument| argument == OsStr::new("core.fsmonitor=false"));
            assert_eq!(
                suppressed,
                fixture.subcommand != "merge",
                "{}: {:?}",
                fixture.subcommand,
                fixture.arguments
            );
        }
    }

    #[test]
    fn fetch_is_confined_to_the_explicit_ref_without_tags_or_submodules() {
        let fetch = update_operation_fixtures()
            .into_iter()
            .find(|fixture| fixture.subcommand == "fetch")
            .expect("fetch fixture");

        for expected in [
            "--porcelain",
            "--dry-run",
            "--no-write-fetch-head",
            "--no-tags",
            "--no-prune",
            "--no-prune-tags",
            "--recurse-submodules=no",
            "--refmap=",
        ] {
            assert!(
                fetch
                    .arguments
                    .iter()
                    .any(|arg| arg == OsStr::new(expected)),
                "missing {expected}"
            );
        }
    }

    #[test]
    fn mandatory_batch_mode_precedes_user_ssh_options() {
        assert_eq!(
            force_ssh_batch_mode("ssh -o BatchMode=no -i key").unwrap(),
            "ssh -o BatchMode=yes -o BatchMode=no -i key"
        );
        assert_eq!(
            force_ssh_batch_mode("'/path with spaces/ssh' -F config").unwrap(),
            "'/path with spaces/ssh' -o BatchMode=yes -F config"
        );
        for compound in [
            "env SSH_AUTH_SOCK=/tmp/agent ssh",
            "VAR=value ssh",
            "exec ssh",
        ] {
            assert!(force_ssh_batch_mode(compound).is_err(), "{compound}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn git_errors_never_retain_transport_diagnostics() {
        let output = Output {
            status: std::process::ExitStatus::from_raw(1 << 8),
            stdout: Vec::new(),
            stderr: b"fatal: https://user:secret@example.test/repository?token=pat".to_vec(),
        };

        let error = git_error(Path::new("/repository"), &["fetch".into()], &output).to_string();

        assert!(error.contains("status 1"), "{error}");
        assert!(error.contains("diagnostic was omitted"), "{error}");
        for secret in ["user", "secret", "token", "pat", "example.test"] {
            assert!(!error.contains(secret), "{error}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn git_errors_retain_only_a_controlled_actionable_classification() {
        let output = Output {
            status: failure_status(),
            stdout: Vec::new(),
            stderr: b"fatal: could not resolve host: secret.example.test".to_vec(),
        };

        let error = git_error(Path::new("/repository"), &["fetch".into()], &output).to_string();

        assert!(
            error.contains("remote host could not be resolved"),
            "{error}"
        );
        assert!(!error.contains("secret.example.test"), "{error}");
    }

    #[test]
    fn git_commands_remove_inherited_repository_routing() {
        let command = command(Path::new("/registered"), &UpdateOp::Merge("abc".into()));
        let removed = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .collect::<HashSet<_>>();

        for key in REPOSITORY_ROUTING_ENVIRONMENT {
            assert!(removed.contains(*key), "{key} was inherited");
        }
        for key in [
            "GIT_CONFIG_SYSTEM",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_NOSYSTEM",
        ] {
            assert!(
                !removed.contains(key),
                "{key} should remain selected by the user"
            );
        }
    }

    #[cfg(unix)]
    fn failure_status() -> std::process::ExitStatus {
        std::process::ExitStatus::from_raw(1 << 8)
    }
}
