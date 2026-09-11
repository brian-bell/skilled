//! A one-shot, object-database-only view of an adopted origin.
//!
//! Each check owns one managed bare repository until its object reads finish.
//! The cache manager serializes checks, proves identity and process inactivity,
//! and reclaims idle objects. Legacy/unproven directories are preserved.

mod cache;

use super::{
    GitTarget, REPOSITORY_ROUTING_ENVIRONMENT, RepositoryHandle, bind_to_handle,
    collect_cancellable_child_strict, effective_remote_url_cancellable, force_ssh_batch_mode,
    permitted_transports_cancellable, remote_url_runs_a_helper, reported_revision,
    repository_is_partial_clone_cancellable, repository_transport_code_cancellable,
    user_ssh_command_cancellable,
};
use crate::provenance::{Origin, validate_update_ref};
use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MAX_ENTRIES: usize = 16_384;
const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_PACK_BYTES: usize = MAX_BYTES;
const MAX_DEPTH: usize = 32;
const MAX_PATH_BYTES: usize = 16 * 1024;
const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
// Aggregate path metadata has its own budget, independent of blob content.
const MAX_TREE_OUTPUT_BYTES: usize = 2 * 1024 * 1024;

/// A regular file from the selected origin subtree. Paths are relative to the
/// selected skill root and have already passed the snapshot safety checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OriginEntry {
    pub path: PathBuf,
    pub executable: bool,
    pub bytes: Vec<u8>,
}

/// The exact commit reported by the fetch and its bounded regular-file tree.
/// Git does not store empty directories, so this representation has no empty
/// directory entries; an update treats their absence as Git's source state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OriginSnapshot {
    pub revision: String,
    pub entries: Vec<OriginEntry>,
    /// Applicable root and intermediate ancestor notices, addressed by the
    /// root-relative filename an update must preserve.
    pub notices: Vec<OriginEntry>,
}

/// Explicitly fetch `update_ref` from a confirmed GitHub origin into a fresh,
/// private bare cache and materialize only its selected subtree. `None` means
/// cancellation; every malformed or unsafe candidate is a refusal.
pub(crate) fn fetch_snapshot(
    cache_root: &Path,
    origin: &Origin,
    update_ref: &str,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> std::result::Result<Option<OriginSnapshot>, String> {
    validate_update_ref(update_ref)?;
    fetch_snapshot_from_url(
        cache_root,
        origin.repository(),
        origin.subdirectory(),
        update_ref,
        cancelled,
        child_slot,
    )
}

fn fetch_snapshot_from_url(
    cache_root: &Path,
    origin_url: &str,
    subdirectory: &str,
    update_ref: &str,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> std::result::Result<Option<OriginSnapshot>, String> {
    if subdirectory != "." && subdirectory.split('/').count() > MAX_DEPTH {
        return Err(format!(
            "origin snapshot exceeds {MAX_DEPTH} directory levels"
        ));
    }
    if cancelled.load(Ordering::Acquire) {
        return Ok(None);
    }
    let cache = cache::Cache::acquire(cache_root)?;
    let result = (|| {
        let handle = cache.repository()?;
        fetch_in_cache(
            &handle,
            origin_url,
            subdirectory,
            update_ref,
            cancelled,
            child_slot,
        )
    })();
    let cleanup = cache.finish();
    match (result, cleanup) {
        (result, Ok(())) => result,
        (Err(error), Err(cleanup)) => Err(format!("{error}; {cleanup}")),
        (Ok(_), Err(cleanup)) => Err(cleanup),
    }
}

fn fetch_in_cache(
    handle: &RepositoryHandle,
    origin_url: &str,
    subdirectory: &str,
    update_ref: &str,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> std::result::Result<Option<OriginSnapshot>, String> {
    // An empty in-cache template is passed relatively: every Git invocation
    // enters the held directory, so no template pathname is re-resolved.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    cache::create_template(handle)?;
    let template = "empty-template";
    run_required(
        handle,
        ["init", "--bare", "--quiet", "--template", template, "."],
        cancelled,
        child_slot,
    )?;
    if cancelled.load(Ordering::Acquire) {
        return Ok(None);
    }
    let Some(configured) = run(
        handle,
        ["config", "remote.origin.url", origin_url],
        cancelled,
        child_slot,
        "https",
        MAX_OUTPUT_BYTES,
        None,
    )?
    else {
        return Ok(None);
    };
    if !configured.status.success() {
        return Err("cannot configure origin cache remote".into());
    }
    let Some(code) =
        repository_transport_code_cancellable(GitTarget::bare(handle), cancelled, child_slot)
            .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    if code.is_some() {
        return Err("origin cache names an unsupported transport program".into());
    }
    let Some(code) = super::repository_windows_unsetenvvars_code_cancellable(
        GitTarget::bare(handle),
        cancelled,
        child_slot,
    )
    .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    if code.is_some() {
        return Err("origin cache removes required child-process guards".into());
    }
    let Some(effective) =
        effective_remote_url_cancellable(GitTarget::bare(handle), "origin", cancelled, child_slot)
            .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    if effective.as_deref().is_none_or(remote_url_runs_a_helper) {
        return Err("origin URL selects an unsupported transport helper".into());
    }
    let Some(allowed_protocols) =
        permitted_transports_cancellable(GitTarget::bare(handle), cancelled, child_slot)
            .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let configured_ssh = if std::env::var("GIT_SSH_COMMAND").is_ok() {
        std::env::var("GIT_SSH_COMMAND").ok()
    } else {
        let Some(value) =
            user_ssh_command_cancellable(GitTarget::bare(handle), cancelled, child_slot)
                .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        value
    };
    let ssh_command = force_ssh_batch_mode(configured_ssh.as_deref().unwrap_or("ssh"))
        .map_err(|error| error.to_string())?;

    let destination = format!("refs/skilled/origin/{}", unique_suffix());
    let refspec = format!("{update_ref}:{destination}");
    // With auxiliary bundles disabled by `run`, dry-run transfers missing
    // objects without ref publication. The real-origin tests verify that a
    // fresh cache can read the reported commit without a second fetch.
    let fetch = run(
        handle,
        [
            "fetch",
            "--porcelain",
            "--dry-run",
            "--no-auto-maintenance",
            "--no-write-fetch-head",
            "--no-tags",
            "--no-prune",
            "--recurse-submodules=no",
            "--depth=1",
            "--refmap=",
            "--",
            origin_url,
            &refspec,
        ],
        cancelled,
        child_slot,
        &allowed_protocols,
        MAX_OUTPUT_BYTES,
        Some(&ssh_command),
    )?;
    let Some(fetch) = fetch else { return Ok(None) };
    if !fetch.status.success() {
        return Err("origin fetch failed".into());
    }
    let revision = reported_revision(&fetch.stdout, &destination)
        .ok_or("origin fetch did not report one full revision")?;
    // Ask before every object read as well as after the transfer: older Git
    // versions need not honour `GIT_NO_LAZY_FETCH` for all plumbing reads.
    let Some(partial) =
        repository_is_partial_clone_cancellable(GitTarget::bare(handle), cancelled, child_slot)
            .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    if partial {
        return Err("origin cache became a partial clone".into());
    }
    let Some(exists) = run(
        handle,
        ["cat-file", "-e", &format!("{revision}^{{commit}}")],
        cancelled,
        child_slot,
        &allowed_protocols,
        MAX_OUTPUT_BYTES,
        None,
    )?
    else {
        return Ok(None);
    };
    if !exists.status.success() {
        return Err("origin fetch reported a commit that is unavailable locally".into());
    }
    // A fresh cache must never become a partial clone: every subsequent object
    // read is local and `GIT_NO_LAZY_FETCH` below is a second boundary.
    let Some(partial) =
        repository_is_partial_clone_cancellable(GitTarget::bare(handle), cancelled, child_slot)
            .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    if partial {
        return Err("origin cache became a partial clone".into());
    }
    let mut tree_budget = MAX_TREE_OUTPUT_BYTES;
    let mut snapshot = snapshot_tree(
        handle,
        &revision,
        subdirectory,
        cancelled,
        child_slot,
        &allowed_protocols,
        &mut tree_budget,
    )?;
    snapshot.notices = ancestor_notices(
        handle,
        &revision,
        subdirectory,
        &snapshot.entries,
        cancelled,
        child_slot,
        &allowed_protocols,
        &mut tree_budget,
    )?;
    Ok(Some(snapshot))
}

fn snapshot_tree(
    handle: &RepositoryHandle,
    revision: &str,
    subdirectory: &str,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
    allowed_protocols: &str,
    tree_budget: &mut usize,
) -> std::result::Result<OriginSnapshot, String> {
    let spec = if subdirectory == "." {
        revision.to_owned()
    } else {
        format!("{revision}:{subdirectory}")
    };
    let Some(listing) = run(
        handle,
        ["ls-tree", "-r", "-z", &spec],
        cancelled,
        child_slot,
        allowed_protocols,
        *tree_budget,
        None,
    )?
    else {
        return Err("origin check cancelled".into());
    };
    if !listing.status.success() {
        return Err("selected origin subdirectory is unavailable".into());
    }
    *tree_budget = tree_budget.saturating_sub(listing.stdout.len());
    let mut entries = Vec::new();
    let mut total = 0usize;
    let mut names = HashSet::new();
    for record in listing
        .stdout
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        if entries.len() == MAX_ENTRIES {
            return Err(format!("origin snapshot exceeds {MAX_ENTRIES} entries"));
        }
        let separator = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or("invalid origin tree entry")?;
        let (header, raw_path) = (&record[..separator], &record[separator + 1..]);
        let mut parts = header.split(|byte| *byte == b' ');
        let mode = parts.next().ok_or("invalid origin tree mode")?;
        let kind = parts.next().ok_or("invalid origin tree kind")?;
        let object = parts.next().ok_or("invalid origin tree object")?;
        if parts.next().is_some() {
            return Err("invalid origin tree entry".into());
        }
        let executable = match (mode, kind) {
            (b"100644", b"blob") => false,
            (b"100755", b"blob") => true,
            (b"120000", _) => return Err("origin snapshot contains a symbolic link".into()),
            (b"160000", _) => return Err("origin snapshot contains a gitlink".into()),
            _ => return Err("origin snapshot contains an unsupported entry mode".into()),
        };
        let path = safe_relative_path(raw_path)?;
        let identity = path.to_string_lossy().to_ascii_lowercase();
        if !names.insert(identity) {
            return Err("origin snapshot contains duplicate or case-colliding paths".into());
        }
        let object = std::str::from_utf8(object).map_err(|_| "origin object name is not UTF-8")?;
        let Some(blob) = run(
            handle,
            ["cat-file", "blob", object],
            cancelled,
            child_slot,
            allowed_protocols,
            MAX_BYTES.saturating_sub(total) + 1,
            None,
        )?
        else {
            return Err("origin check cancelled".into());
        };
        if !blob.status.success() {
            return Err("origin blob is unavailable locally".into());
        }
        total = total
            .checked_add(blob.stdout.len())
            .ok_or("origin snapshot size overflow")?;
        if total > MAX_BYTES {
            return Err(format!("origin snapshot exceeds {MAX_BYTES} bytes"));
        }
        entries.push(OriginEntry {
            path,
            executable,
            bytes: blob.stdout,
        });
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(OriginSnapshot {
        revision: revision.into(),
        entries,
        notices: Vec::new(),
    })
}

// Keep the shared metadata budget explicit beside the existing cancellation
// and transport guards; this reader has no independent mutable configuration.
#[expect(clippy::too_many_arguments)]
fn ancestor_notices(
    handle: &RepositoryHandle,
    revision: &str,
    subdirectory: &str,
    selected: &[OriginEntry],
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
    allowed_protocols: &str,
    tree_budget: &mut usize,
) -> std::result::Result<Vec<OriginEntry>, String> {
    if subdirectory == "." {
        return Ok(Vec::new());
    }
    let mut ancestors = vec![String::new()];
    let mut current = String::new();
    let components = subdirectory.split('/').collect::<Vec<_>>();
    for component in &components[..components.len().saturating_sub(1)] {
        if !current.is_empty() {
            current.push('/')
        }
        current.push_str(component);
        ancestors.push(current.clone());
    }
    let mut notices = Vec::new();
    let mut destinations = HashSet::new();
    let mut used_bytes = selected
        .iter()
        .map(|entry| entry.bytes.len())
        .sum::<usize>();
    for ancestor in ancestors {
        let spec = if ancestor.is_empty() {
            revision.to_owned()
        } else {
            format!("{revision}:{ancestor}")
        };
        let Some(listing) = run(
            handle,
            ["ls-tree", "-z", &spec],
            cancelled,
            child_slot,
            allowed_protocols,
            *tree_budget,
            None,
        )?
        else {
            return Err("origin check cancelled".into());
        };
        if !listing.status.success() {
            return Err("origin ancestor directory is unavailable".into());
        }
        *tree_budget = tree_budget.saturating_sub(listing.stdout.len());
        for record in listing
            .stdout
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
        {
            let separator = record
                .iter()
                .position(|byte| *byte == b'\t')
                .ok_or("invalid origin ancestor entry")?;
            let (header, name) = (&record[..separator], &record[separator + 1..]);
            if !notice_name(name)? {
                continue;
            }
            let mut parts = header.split(|byte| *byte == b' ');
            let mode = parts.next().ok_or("invalid origin notice mode")?;
            let kind = parts.next().ok_or("invalid origin notice kind")?;
            let object = parts.next().ok_or("invalid origin notice object")?;
            if parts.next().is_some() {
                return Err("invalid origin notice entry".into());
            }
            if mode == b"120000" {
                return Err("origin notice is a symbolic link".into());
            }
            let executable = match (mode, kind) {
                (b"100644", b"blob") => false,
                (b"100755", b"blob") => true,
                _ => return Err("origin notice has an unsupported entry mode".into()),
            };
            let path = safe_relative_path(name)?;
            let identity = path.to_string_lossy().to_ascii_lowercase();
            let object =
                std::str::from_utf8(object).map_err(|_| "origin notice object is not UTF-8")?;
            let existing = selected
                .iter()
                .chain(notices.iter())
                .find(|entry: &&OriginEntry| {
                    entry.path.to_string_lossy().to_ascii_lowercase() == identity
                });
            let read_limit = existing.map_or(MAX_BYTES.saturating_sub(used_bytes), |entry| {
                entry.bytes.len()
            });
            let Some(blob) = run(
                handle,
                ["cat-file", "blob", object],
                cancelled,
                child_slot,
                allowed_protocols,
                read_limit,
                None,
            )?
            else {
                return Err("origin check cancelled".into());
            };
            if !blob.status.success() {
                return Err("origin notice blob is unavailable locally".into());
            }
            if let Some(existing) = existing {
                if existing.path != path
                    || existing.bytes != blob.stdout
                    || existing.executable != executable
                {
                    return Err("origin notice conflicts with existing notice material".into());
                }
                // Comparison reads do not consume output-tree capacity:
                // selected and repeated ancestor notices are retained once.
                continue;
            }
            used_bytes = used_bytes
                .checked_add(blob.stdout.len())
                .ok_or("origin snapshot size overflow")?;
            if used_bytes > MAX_BYTES {
                return Err(format!("origin snapshot exceeds {MAX_BYTES} bytes"));
            }
            if selected.len() + notices.len() == MAX_ENTRIES {
                return Err(format!("origin snapshot exceeds {MAX_ENTRIES} entries"));
            }
            if !destinations.insert(identity) {
                return Err("origin ancestor notices case-collide".into());
            }
            notices.push(OriginEntry {
                path,
                executable,
                bytes: blob.stdout,
            });
        }
    }
    notices.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(notices)
}

fn notice_name(bytes: &[u8]) -> std::result::Result<bool, String> {
    let name = std::str::from_utf8(bytes).map_err(|_| "origin notice path is not UTF-8")?;
    let lowered = name.to_ascii_lowercase();
    let prefixes = ["license", "licence", "copying", "notice", "attribution"];
    Ok(prefixes.into_iter().any(|prefix| {
        lowered == prefix
            || lowered
                .strip_prefix(prefix)
                .is_some_and(|suffix| suffix.starts_with('.') || suffix.starts_with('-'))
    }))
}

/// Syntactic path safety for a read-only candidate. A future executor must
/// also stage on the destination volume and verify filesystem equivalences
/// (including Unicode normalization) before replacing anything; observing the
/// application cache cannot prove another volume's naming semantics.
pub(crate) fn safe_relative_path(bytes: &[u8]) -> std::result::Result<PathBuf, String> {
    if bytes.is_empty()
        || bytes.len() > MAX_PATH_BYTES
        || bytes.contains(&0)
        || bytes.contains(&b'\\')
    {
        return Err("origin entry path is unsafe".into());
    }
    let value = std::str::from_utf8(bytes).map_err(|_| "origin entry path is not UTF-8")?;
    if value.split('/').any(|component| {
        component.is_empty() || matches!(component, "." | "..") || component.len() > 255
    }) {
        return Err("origin entry has an unsafe filename component".into());
    }
    if value.chars().any(char::is_control) {
        return Err("origin entry path contains control characters".into());
    }
    #[cfg(windows)]
    for component in value.split('/') {
        if !windows_component_is_representable(component) {
            return Err("origin entry path cannot be represented on Windows".into());
        }
    }
    let path = Path::new(value);
    if path.components().count() > MAX_DEPTH
        || !path
            .components()
            .all(|component| matches!(component, Component::Normal(part) if !part.is_empty() && !part.eq_ignore_ascii_case(".git")))
    {
        return Err("origin entry path is unsafe".into());
    }
    Ok(path.to_path_buf())
}

#[cfg(any(windows, test))]
fn windows_component_is_representable(component: &str) -> bool {
    if component.ends_with(['.', ' '])
        || component
            .chars()
            .any(|character| matches!(character, ':' | '<' | '>' | '"' | '|' | '?' | '*'))
    {
        return false;
    }
    let stem = component
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_uppercase();
    !matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) && !["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}

fn run_required<const N: usize>(
    handle: &RepositoryHandle,
    arguments: [&str; N],
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> std::result::Result<(), String> {
    let Some(output) = run(
        handle,
        arguments,
        cancelled,
        child_slot,
        "https",
        MAX_OUTPUT_BYTES,
        None,
    )?
    else {
        return Ok(());
    };
    output
        .status
        .success()
        .then_some(())
        .ok_or_else(|| "cannot initialize origin cache".into())
}

fn run<const N: usize>(
    handle: &RepositoryHandle,
    arguments: [&str; N],
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
    allowed_protocols: &str,
    output_limit: usize,
    ssh_command: Option<&str>,
) -> std::result::Result<Option<Output>, String> {
    let mut command = Command::new("git");
    for key in REPOSITORY_ROUTING_ENVIRONMENT {
        command.env_remove(key);
    }
    #[cfg(test)]
    if let Some(config) = super::TEST_GIT_CONFIG_GLOBAL.with(|value| value.borrow().clone()) {
        command.env("GIT_CONFIG_GLOBAL", config);
    }
    bind_to_handle(&mut command, handle);
    command
        .env("GIT_DIR", ".")
        .env("GIT_ALLOW_PROTOCOL", allowed_protocols)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-c")
        .arg(format!("core.hooksPath={}", super::SUPPRESSED_HOOKS_PATH))
        .arg("-c")
        .arg("hook.reference-transaction.enabled=false")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(ssh_command) = ssh_command {
        command.envs(super::fetch_environment(ssh_command, allowed_protocols));
    }
    if arguments.first() == Some(&"fetch") {
        limit_fetch_file_size(&mut command)?;
        command.args(super::suppressed_bundle_arguments());
        command.arg("-c").arg("fetch.unpackLimit=0");
    }
    command.args(arguments);
    let child = command
        .spawn()
        .map_err(|error| format!("cannot start Git origin check: {error}"))?;
    let output = if arguments.first() == Some(&"fetch") {
        collect_fetch_with_cache_budget(child, handle, cancelled, child_slot)?
    } else {
        // Structured origin input stops the child on overflow; never drain a
        // decompression bomb or accept a complete-looking truncated prefix.
        collect_cancellable_child_strict(child, cancelled, child_slot, output_limit)
            .map_err(|error| error.to_string())?
    };
    if output.as_ref().is_some_and(|output| {
        output.stdout.len() > output_limit || output.stderr.len() > output_limit
    }) {
        return Err("origin Git output exceeds its read budget".into());
    }
    Ok(output)
}

#[cfg(unix)]
fn limit_fetch_file_size(command: &mut Command) -> std::result::Result<(), String> {
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| {
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::getrlimit(libc::RLIMIT_FSIZE, &mut limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            limit.rlim_cur = limit.rlim_cur.min(MAX_PACK_BYTES as libc::rlim_t);
            if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn limit_fetch_file_size(_command: &mut Command) -> std::result::Result<(), String> {
    Ok(())
}

fn collect_fetch_with_cache_budget(
    child: Child,
    cache: &RepositoryHandle,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> std::result::Result<Option<Output>, String> {
    // Drain pipes through the shared collector while monitoring the complete
    // private cache. The threshold can overshoot between 10ms observations;
    // Unix additionally imposes a hard limit on each pack file.
    let stopped = AtomicBool::new(false);
    let stop_child = AtomicBool::new(false);
    let failure = Mutex::new(None);
    let result = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !stopped.load(Ordering::Acquire) {
                let problem = match cache_size(cache) {
                    Ok(bytes) if bytes > MAX_PACK_BYTES => {
                        Some("origin transfer exceeds 33554432 bytes".to_owned())
                    }
                    Ok(_) => None,
                    Err(error) => Some(error),
                };
                if problem.is_some() || cancelled.load(Ordering::Acquire) {
                    *failure.lock().unwrap_or_else(|poison| poison.into_inner()) = problem;
                    stop_child.store(true, Ordering::Release);
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let result =
            collect_cancellable_child_strict(child, &stop_child, child_slot, MAX_OUTPUT_BYTES)
                .map_err(|error| error.to_string());
        stopped.store(true, Ordering::Release);
        result
    });
    if let Some(error) = failure
        .into_inner()
        .unwrap_or_else(|poison| poison.into_inner())
    {
        return Err(error);
    }
    if cancelled.load(Ordering::Acquire) {
        return Ok(None);
    }
    // The child can finish between observations; never accept an over-budget
    // cache merely because it completed before the next monitor tick.
    if cache_size(cache)? > MAX_PACK_BYTES {
        return Err("origin transfer exceeds 33554432 bytes".into());
    }
    result
}

fn cache_size(handle: &RepositoryHandle) -> std::result::Result<usize, String> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        cache::size(handle)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = handle;
        Err("origin cache inspection is unsupported".into())
    }
}

fn unique_suffix() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{:x}-{nanos:x}-{:x}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, process::Command, sync::atomic::AtomicBool};
    use tempfile::TempDir;

    #[cfg(unix)]
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn monitored_fetch_drains_both_pipes_without_waiting_for_exit() {
        let temporary = TempDir::new().unwrap();
        let child = Command::new("sh")
            .args([
                "-c",
                "head -c 262144 /dev/zero; head -c 262144 /dev/zero >&2",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let result = collect_fetch_with_cache_budget(
            child,
            &RepositoryHandle::open(temporary.path()).unwrap(),
            &AtomicBool::new(false),
            &Mutex::new(None),
        )
        .unwrap()
        .unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout.len(), 262144);
        assert_eq!(result.stderr.len(), 262144);
    }

    #[cfg(unix)]
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn monitored_fetch_terminates_child_when_cache_exceeds_budget() {
        let temporary = TempDir::new().unwrap();
        fs::File::create(temporary.path().join("pack"))
            .unwrap()
            .set_len(MAX_PACK_BYTES as u64 + 1)
            .unwrap();
        let child = Command::new("sleep")
            .arg("30")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        let result = collect_fetch_with_cache_budget(
            child,
            &RepositoryHandle::open(temporary.path()).unwrap(),
            &AtomicBool::new(false),
            &Mutex::new(None),
        );
        assert!(result.unwrap_err().contains("exceeds"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn rejects_windows_device_stream_and_alias_names() {
        for name in [
            "CON",
            "nul.txt",
            "COM1.md",
            "LPT²",
            "file:stream",
            "name.",
            "name ",
            "a*b",
            "a?b",
        ] {
            assert!(!windows_component_is_representable(name), "{name}");
        }
        for name in ["SKILL.md", "scripts", "company.txt", "COM10", "café"] {
            assert!(windows_component_is_representable(name), "{name}");
        }
    }

    fn git(directory: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .status()
            .expect("run git");
        assert!(status.success(), "git {arguments:?}");
    }

    fn fixture() -> (TempDir, PathBuf) {
        let temporary = TempDir::new().expect("temporary directory");
        let remote = temporary.path().join("origin.git");
        let source = temporary.path().join("source");
        fs::create_dir(&source).expect("source");
        git(&source, &["init", "--quiet", "-b", "main"]);
        git(&source, &["config", "user.email", "test@example.test"]);
        git(&source, &["config", "user.name", "Test"]);
        fs::create_dir_all(source.join("skills/demo")).expect("skill");
        fs::write(source.join("LICENSE"), "upstream license\n").expect("license");
        fs::write(
            source.join("skills/demo/SKILL.md"),
            "---\nname: demo\n---\n",
        )
        .expect("skill document");
        fs::write(source.join("skills/demo/run"), "#!/bin/sh\n").expect("executable");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                source.join("skills/demo/run"),
                fs::Permissions::from_mode(0o755),
            )
            .expect("mode");
        }
        git(&source, &["add", "."]);
        git(&source, &["commit", "--quiet", "-m", "initial"]);
        Command::new("git")
            .args([
                "init",
                "--bare",
                "--quiet",
                remote.to_str().expect("remote path"),
            ])
            .status()
            .expect("bare remote");
        git(
            &source,
            &[
                "remote",
                "add",
                "origin",
                remote.to_str().expect("remote path"),
            ],
        );
        git(&source, &["push", "--quiet", "origin", "main"]);
        (temporary, remote)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn rewritten_snapshot(
        temporary: &TempDir,
        remote: &Path,
        deny_file: bool,
    ) -> std::result::Result<Option<OriginSnapshot>, String> {
        // The production entry point still validates a GitHub HTTPS URL. This
        // scoped user configuration merely lets the unit fixture map that URL
        // to its local bare repository without modifying user configuration.
        let config = temporary.path().join("test.gitconfig");
        let file_policy = if deny_file {
            "[protocol \"file\"]\n\tallow = never\n"
        } else {
            ""
        };
        fs::write(
            &config,
            format!(
                "[url \"file://{}\"]\n\tinsteadOf = https://github.com/example/demo\n{file_policy}",
                remote.display()
            ),
        )
        .expect("rewrite config");
        super::super::TEST_GIT_CONFIG_GLOBAL.with(|value| {
            *value.borrow_mut() = Some(config.as_os_str().to_owned());
        });
        let result = fetch_snapshot(
            &temporary.path().join("cache"),
            &Origin::new(
                "https://github.com/example/demo".into(),
                "skills/demo".into(),
            )
            .expect("origin"),
            "refs/heads/main",
            &AtomicBool::new(false),
            &Mutex::new(None),
        );
        super::super::TEST_GIT_CONFIG_GLOBAL.with(|value| *value.borrow_mut() = None);
        // Both accepted and refused real fetches retire their object database.
        assert_idle_cache(&temporary.path().join("cache"));
        result
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn assert_idle_cache(path: &Path) {
        let mut entries = fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        entries.sort();
        assert_eq!(
            entries,
            ["activity.lock", "manager.lock", "owner.json"].map(std::ffi::OsString::from)
        );
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn repeated_real_checks_keep_snapshots_usable_after_reclamation() {
        let (temporary, remote) = fixture();
        let first = rewritten_snapshot(&temporary, &remote, false)
            .unwrap()
            .unwrap();
        assert_eq!(
            rewritten_snapshot(&temporary, &remote, true).unwrap_err(),
            "origin fetch failed"
        );
        let second = rewritten_snapshot(&temporary, &remote, false)
            .unwrap()
            .unwrap();
        assert_eq!(first, second);
        assert!(!first.entries[0].bytes.is_empty());
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn cancellation_after_allocation_reclaims_the_cache() {
        use std::sync::Arc;
        let (temporary, remote) = fixture();
        let cache = temporary.path().join("cache");
        let config = temporary.path().join("empty.gitconfig");
        fs::write(&config, "").unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        let worker_cache = cache.clone();
        let worker = std::thread::spawn(move || {
            super::super::TEST_GIT_CONFIG_GLOBAL
                .with(|value| *value.borrow_mut() = Some(config.into_os_string()));
            fetch_snapshot_from_url(
                &worker_cache,
                &format!("file://{}", remote.display()),
                "skills/demo",
                "refs/heads/main",
                &worker_cancelled,
                &Mutex::new(None),
            )
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !cache.join("repository").exists() && !worker.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "cache was not allocated"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        cancelled.store(true, Ordering::Release);
        assert!(!matches!(worker.join().unwrap(), Ok(Some(_))));
        assert_idle_cache(&cache);
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn fetches_a_relative_regular_file_snapshot_from_an_https_origin() {
        let (temporary, remote) = fixture();
        let snapshot = rewritten_snapshot(&temporary, &remote, false)
            .expect("snapshot")
            .expect("not cancelled");
        assert_eq!(snapshot.entries.len(), 2);
        assert_eq!(snapshot.entries[0].path, Path::new("SKILL.md"));
        assert!(!snapshot.entries[0].executable);
        assert_eq!(snapshot.entries[1].path, Path::new("run"));
        assert!(snapshot.entries[1].executable);
        assert_eq!(snapshot.notices.len(), 1);
        assert_eq!(snapshot.notices[0].path, Path::new("LICENSE"));
    }

    #[test]
    #[cfg(unix)]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn rejects_a_symbolic_link_in_the_selected_tree() {
        let (temporary, remote) = fixture();
        let source = temporary.path().join("source");
        std::os::unix::fs::symlink("SKILL.md", source.join("skills/demo/link")).expect("link");
        git(&source, &["add", "."]);
        git(&source, &["commit", "--quiet", "-m", "link"]);
        git(&source, &["push", "--quiet", "origin", "main"]);
        let error =
            rewritten_snapshot(&temporary, &remote, false).expect_err("symlink must refuse");
        assert!(error.contains("symbolic link"), "{error}");
    }

    #[test]
    fn cancellation_does_not_create_a_cache() {
        let temporary = TempDir::new().expect("temporary");
        let result = fetch_snapshot(
            &temporary.path().join("cache"),
            &Origin::new("https://github.com/example/demo".into(), ".".into()).expect("origin"),
            "refs/heads/main",
            &AtomicBool::new(true),
            &Mutex::new(None),
        )
        .expect("cancelled");
        assert!(result.is_none());
        assert!(!temporary.path().join("cache").exists());
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn rejects_conflicting_notices_from_nested_ancestors() {
        let (temporary, remote) = fixture();
        let source = temporary.path().join("source");
        fs::write(source.join("skills/LICENSE"), "different license\n").expect("nested license");
        git(&source, &["add", "."]);
        git(&source, &["commit", "--quiet", "-m", "nested license"]);
        git(&source, &["push", "--quiet", "origin", "main"]);
        let error =
            rewritten_snapshot(&temporary, &remote, false).expect_err("conflict must refuse");
        assert!(
            error.contains("notices disagree") || error.contains("output exceeds"),
            "{error}"
        );
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn identical_selected_and_ancestor_license_is_kept_once() {
        let (temporary, remote) = fixture();
        let source = temporary.path().join("source");
        fs::write(source.join("skills/demo/LICENSE"), "upstream license\n")
            .expect("selected license");
        git(&source, &["add", "."]);
        git(&source, &["commit", "--quiet", "-m", "same license"]);
        git(&source, &["push", "--quiet", "origin", "main"]);
        let snapshot = rewritten_snapshot(&temporary, &remote, false)
            .expect("snapshot")
            .expect("not cancelled");
        assert!(
            snapshot
                .entries
                .iter()
                .any(|entry| entry.path == Path::new("LICENSE"))
        );
        assert!(
            snapshot.notices.is_empty(),
            "selected license must not duplicate an ancestor notice"
        );
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn differing_selected_and_ancestor_license_blocks_the_snapshot() {
        let (temporary, remote) = fixture();
        let source = temporary.path().join("source");
        fs::write(
            source.join("skills/demo/LICENSE"),
            "different selected license\n",
        )
        .expect("selected license");
        git(&source, &["add", "."]);
        git(&source, &["commit", "--quiet", "-m", "different license"]);
        git(&source, &["push", "--quiet", "origin", "main"]);
        let error = rewritten_snapshot(&temporary, &remote, false)
            .expect_err("different selected license must refuse");
        assert!(
            error.contains("conflicts") || error.contains("disagree"),
            "{error}"
        );
    }

    #[test]
    #[cfg(unix)]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn rejects_a_symbolic_link_notice() {
        let (temporary, remote) = fixture();
        let source = temporary.path().join("source");
        fs::remove_file(source.join("LICENSE")).expect("remove license");
        std::os::unix::fs::symlink("skills/demo/SKILL.md", source.join("LICENSE"))
            .expect("notice link");
        git(&source, &["add", "."]);
        git(&source, &["commit", "--quiet", "-m", "notice link"]);
        git(&source, &["push", "--quiet", "origin", "main"]);
        let error =
            rewritten_snapshot(&temporary, &remote, false).expect_err("notice link must refuse");
        assert!(error.contains("notice is a symbolic link"), "{error}");
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn user_transport_policy_bounds_a_rewritten_origin_fetch() {
        let (temporary, remote) = fixture();
        let error = rewritten_snapshot(&temporary, &remote, true)
            .expect_err("user protocol policy must refuse the file rewrite");
        assert_eq!(error, "origin fetch failed");
    }

    #[test]
    fn bare_handle_reads_its_own_transport_configuration() {
        let temporary = TempDir::new().expect("temporary");
        let cache = temporary.path().join("cache");
        fs::create_dir(&cache).expect("cache");
        Command::new("git")
            .args([
                "-C",
                cache.to_str().expect("cache"),
                "init",
                "--bare",
                "--quiet",
            ])
            .status()
            .expect("init bare");
        Command::new("git")
            .args([
                "-C",
                cache.to_str().expect("cache"),
                "config",
                "core.sshCommand",
                "unsafe",
            ])
            .status()
            .expect("set local config");
        let handle = RepositoryHandle::open(&cache).expect("handle");
        let code = repository_transport_code_cancellable(
            GitTarget::bare(&handle),
            &AtomicBool::new(false),
            &Mutex::new(None),
        )
        .expect("guard")
        .expect("not cancelled");
        assert_eq!(code.as_deref(), Some("core.sshcommand"));
    }

    #[test]
    fn tree_listing_rejects_a_complete_first_record_when_more_output_follows() {
        let (_temporary, remote) = fixture();
        let handle = RepositoryHandle::open(&remote).expect("bare handle");
        let revision = String::from_utf8(
            Command::new("git")
                .args([
                    "-C",
                    remote.to_str().expect("remote"),
                    "rev-parse",
                    "refs/heads/main",
                ])
                .output()
                .expect("revision")
                .stdout,
        )
        .expect("revision text")
        .trim()
        .to_owned();
        let listing = Command::new("git")
            .args([
                "-C",
                remote.to_str().expect("remote"),
                "ls-tree",
                "-z",
                &revision,
            ])
            .output()
            .expect("listing")
            .stdout;
        let first_record = listing
            .iter()
            .position(|byte| *byte == 0)
            .expect("first record")
            + 1;
        let error = run(
            &handle,
            ["ls-tree", "-z", &revision],
            &AtomicBool::new(false),
            &Mutex::new(None),
            "file",
            first_record,
            None,
        )
        .expect_err("an additional record must not be silently truncated");
        assert!(error.contains("output exceeds"), "{error}");
    }

    #[test]
    fn duplicate_selected_notice_does_not_consume_budget_twice() {
        let (_temporary, remote) = fixture();
        let handle = RepositoryHandle::open(&remote).expect("bare handle");
        let revision = String::from_utf8(
            Command::new("git")
                .args([
                    "-C",
                    remote.to_str().expect("remote"),
                    "rev-parse",
                    "refs/heads/main",
                ])
                .output()
                .expect("revision")
                .stdout,
        )
        .expect("revision text")
        .trim()
        .to_owned();
        let license = b"upstream license\n".to_vec();
        let selected = vec![
            OriginEntry {
                path: PathBuf::from("payload"),
                executable: false,
                bytes: vec![7; MAX_BYTES - license.len()],
            },
            OriginEntry {
                path: PathBuf::from("LICENSE"),
                executable: false,
                bytes: license,
            },
        ];
        let mut tree_budget = MAX_TREE_OUTPUT_BYTES;
        let notices = ancestor_notices(
            &handle,
            &revision,
            "skills/demo",
            &selected,
            &AtomicBool::new(false),
            &Mutex::new(None),
            "file",
            &mut tree_budget,
        )
        .expect("identical selected notice must not exceed the aggregate budget");
        assert!(notices.is_empty());
    }

    #[test]
    fn identical_notices_from_multiple_ancestors_are_retained_once() {
        let (temporary, remote) = fixture();
        let source = temporary.path().join("source");
        fs::write(source.join("skills/LICENSE"), "upstream license\n").expect("nested license");
        git(&source, &["add", "."]);
        git(
            &source,
            &["commit", "--quiet", "-m", "duplicate ancestor license"],
        );
        git(&source, &["push", "--quiet", "origin", "main"]);
        let handle = RepositoryHandle::open(&remote).expect("bare handle");
        let revision = String::from_utf8(
            Command::new("git")
                .args([
                    "-C",
                    remote.to_str().expect("remote"),
                    "rev-parse",
                    "refs/heads/main",
                ])
                .output()
                .expect("revision")
                .stdout,
        )
        .expect("revision text")
        .trim()
        .to_owned();
        let mut tree_budget = MAX_TREE_OUTPUT_BYTES;
        let notices = ancestor_notices(
            &handle,
            &revision,
            "skills/demo",
            &[],
            &AtomicBool::new(false),
            &Mutex::new(None),
            "file",
            &mut tree_budget,
        )
        .expect("notices");
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].path, Path::new("LICENSE"));
    }

    #[test]
    fn ancestor_listing_uses_the_remaining_shared_tree_budget() {
        let (_temporary, remote) = fixture();
        let handle = RepositoryHandle::open(&remote).expect("bare handle");
        let revision = String::from_utf8(
            Command::new("git")
                .args([
                    "-C",
                    remote.to_str().expect("remote"),
                    "rev-parse",
                    "refs/heads/main",
                ])
                .output()
                .expect("revision")
                .stdout,
        )
        .expect("revision text")
        .trim()
        .to_owned();
        let mut remaining = 1;
        let error = ancestor_notices(
            &handle,
            &revision,
            "skills/demo",
            &[],
            &AtomicBool::new(false),
            &Mutex::new(None),
            "file",
            &mut remaining,
        )
        .expect_err("ancestor tree output cannot exceed the shared remainder");
        assert!(error.contains("output exceeds"), "{error}");
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn rejects_an_incompressible_origin_transfer_before_snapshotting() {
        let (temporary, remote) = fixture();
        let source = temporary.path().join("source");
        let mut bytes = vec![0_u8; MAX_PACK_BYTES + 1024 * 1024];
        let mut state = 0x9e37_79b9_u32;
        for byte in &mut bytes {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *byte = (state >> 24) as u8;
        }
        fs::write(source.join("skills/demo/oversized.bin"), bytes).expect("oversized fixture");
        git(&source, &["add", "."]);
        git(&source, &["commit", "--quiet", "-m", "oversized"]);
        git(&source, &["push", "--quiet", "origin", "main"]);
        let error = rewritten_snapshot(&temporary, &remote, false)
            .expect_err("oversized transfer must refuse");
        assert!(
            error.contains("transfer exceeds") || error == "origin fetch failed",
            "{error}"
        );
    }
}
