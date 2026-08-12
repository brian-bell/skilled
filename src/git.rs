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
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
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
    "merge",
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
    revision: String,
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
    pub fn revision(&self) -> &str {
        &self.revision
    }

    pub(crate) fn with_revision(&self, revision: String) -> Self {
        let mut fetched = self.clone();
        fetched.revision = revision;
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
    ConfigGet(String),
    FilterSettings,
    PromisorSettings,
    CheckAttr,
    MergeBase(String, String),
    AheadBehind(String, String),
    ChangedPaths(String, String),
    CommitSummaries(String, String),
    CatFile(String),
    TreeEntry {
        revision: String,
        path: PathBuf,
    },
    Status,
    Fetch {
        remote: String,
        refspec: String,
        ssh_command: String,
        hooks_path: PathBuf,
    },
    Merge(String),
}

impl UpdateOp {
    fn subcommand(&self) -> &'static str {
        match self {
            Self::RevParse(_) | Self::AbsoluteGitDir => "rev-parse",
            Self::SymbolicHead | Self::SymbolicRef(_) => "symbolic-ref",
            Self::UpstreamRef(_) => "for-each-ref",
            Self::ConfigGet(_) | Self::FilterSettings | Self::PromisorSettings => "config",
            Self::CheckAttr => "check-attr",
            Self::MergeBase(_, _) => "merge-base",
            Self::AheadBehind(_, _) | Self::CommitSummaries(_, _) => "rev-list",
            Self::ChangedPaths(_, _) => "diff-tree",
            Self::CatFile(_) => "cat-file",
            Self::TreeEntry { .. } => "ls-tree",
            Self::Status => "status",
            Self::Fetch { .. } => "fetch",
            Self::Merge(_) => "merge",
        }
    }

    fn arguments(&self) -> Vec<OsString> {
        // A fetch updates the remote-tracking ref, and Git runs the
        // repository's `reference-transaction` hook for that update. A check
        // the user was told only reads must not run repository code, so the
        // hook search is pointed at a path inside the Git directory that
        // Skilled never creates; anything able to plant a hook there could
        // plant one in `hooks` itself.
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
                "--no-auto-maintenance".into(),
                // The tracking ref is the whole destination: rewriting
                // FETCH_HEAD would discard state the user's own fetch left
                // there, which a check is not entitled to touch.
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
        if let Self::TreeEntry { revision, path } = self {
            return vec![
                "ls-tree".into(),
                "-d".into(),
                "--name-only".into(),
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
            Self::ConfigGet(key) => vec!["config".into(), "--get".into(), key.clone()],
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
            Self::TreeEntry { .. } => {
                unreachable!("tree operations return above")
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
            Self::Fetch { ssh_command, .. } => vec![
                ("GIT_TERMINAL_PROMPT".into(), "0".into()),
                ("GIT_ASKPASS".into(), "".into()),
                ("SSH_ASKPASS_REQUIRE".into(), "never".into()),
                ("GIT_SSH_COMMAND".into(), ssh_command.into()),
                ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
            ],
            Self::TreeEntry { .. } => vec![
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
    let Some(revision) = optional(repository, UpdateOp::RevParse(tracking_ref.clone()))?
        .map(text)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
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
    let Some(revision) = revision.map(text).filter(|value| !value.is_empty()) else {
        return Ok(Some(None));
    };
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

fn config_get_cancellable(
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

pub fn fetch_upstream(repository: &Path, upstream: &Upstream) -> Result<String> {
    reject_symbolic_tracking_ref(repository, upstream)?;
    let ssh_command = force_ssh_batch_mode(
        &env::var("GIT_SSH_COMMAND")
            .ok()
            .or(config_get(repository, "core.sshCommand")?)
            .unwrap_or_else(|| "ssh".into()),
    )?;
    // The explicit destination keeps the configured remote-tracking ref and
    // the cached upstream revision identical even when the user's ordinary
    // fetch refspec excludes this branch. The leading `+` is required to
    // observe and classify a rewritten upstream as diverged.
    let refspec = upstream_refspec(upstream);
    let op = UpdateOp::Fetch {
        remote: upstream.remote.clone(),
        refspec,
        ssh_command,
        hooks_path: SUPPRESSED_HOOKS_PATH.into(),
    };
    let arguments = op.arguments();
    let output = run_bounded(repository, &op, MAX_FETCH_OUTPUT_BYTES)?;
    if output.status.success() {
        Ok(text(required(
            repository,
            UpdateOp::RevParse(upstream.tracking_ref.clone()),
        )?))
    } else {
        Err(git_error(repository, &arguments, &output))
    }
}

pub fn spawn_fetch_upstream(repository: &Path, upstream: &Upstream) -> Result<Child> {
    reject_symbolic_tracking_ref(repository, upstream)?;
    let ssh_command = force_ssh_batch_mode(
        &env::var("GIT_SSH_COMMAND")
            .ok()
            .or(config_get(repository, "core.sshCommand")?)
            .unwrap_or_else(|| "ssh".into()),
    )?;
    let refspec = upstream_refspec(upstream);
    command(
        repository,
        &UpdateOp::Fetch {
            remote: upstream.remote.clone(),
            refspec,
            ssh_command,
            hooks_path: SUPPRESSED_HOOKS_PATH.into(),
        },
    )
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .map_err(Error::GitUnavailable)
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
        let Some(value) =
            config_get_cancellable(repository, "core.sshCommand", cancelled, child_slot)?
        else {
            return Ok(None);
        };
        value
    };
    let ssh_command = force_ssh_batch_mode(configured_ssh.as_deref().unwrap_or("ssh"))?;
    let op = UpdateOp::Fetch {
        remote: upstream.remote.clone(),
        refspec: upstream_refspec(upstream),
        ssh_command,
        hooks_path: SUPPRESSED_HOOKS_PATH.into(),
    };
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
    Ok(required_cancellable(
        repository,
        UpdateOp::RevParse(upstream.tracking_ref.clone()),
        cancelled,
        child_slot,
    )?
    .map(text))
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

fn upstream_refspec(upstream: &Upstream) -> String {
    format!("+{}:{}", upstream.merge_ref, upstream.tracking_ref)
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

/// Whether `path` names a directory in the exact commit tree. Literal
/// pathspec handling keeps catalog names from becoming Git pattern syntax.
pub fn tree_directory_exists(repository: &Path, revision: &str, path: &Path) -> Result<bool> {
    Ok(!required(
        repository,
        UpdateOp::TreeEntry {
            revision: revision.into(),
            path: path.into(),
        },
    )?
    .is_empty())
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
        UpdateOp::FilterSettings,
        UpdateOp::PromisorSettings,
        UpdateOp::CheckAttr,
        UpdateOp::MergeBase("a".into(), "b".into()),
        UpdateOp::AheadBehind("a".into(), "b".into()),
        UpdateOp::ChangedPaths("a".into(), "b".into()),
        UpdateOp::CommitSummaries("a".into(), "b".into()),
        UpdateOp::CatFile("abc".into()),
        UpdateOp::TreeEntry {
            revision: "abc".into(),
            path: "skills/demo".into(),
        },
        UpdateOp::Status,
        UpdateOp::Fetch {
            remote: "origin".into(),
            refspec: "refs/heads/main".into(),
            ssh_command: "ssh -o BatchMode=yes".into(),
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

    #[test]
    fn fetch_is_confined_to_the_explicit_ref_without_tags_or_submodules() {
        let fetch = update_operation_fixtures()
            .into_iter()
            .find(|fixture| fixture.subcommand == "fetch")
            .expect("fetch fixture");

        for expected in [
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
