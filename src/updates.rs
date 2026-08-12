//! Repository update probing, classification, planning, application and verification.

use std::{
    path::{Path, PathBuf},
    process::Child,
    sync::{Mutex, atomic::AtomicBool},
};

use crate::{
    Result,
    git::{self, ChangeKind, ChangedPath, HeadState, Upstream, WorktreeState},
    inventory::{Finding, FindingSeverity, InventorySnapshot},
    source::{
        CatalogProposal, RegisteredSource, RepositoryIdentity, repository_identity,
        repository_identity_from_git_dir,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepositoryUpdateVerdict {
    UpToDate,
    Ahead,
    Available,
    Blocked,
}

#[cfg(test)]
mod tests {
    use super::gitlink_intersects_catalog;
    use std::path::Path;

    #[test]
    fn gitlinks_intersect_catalogs_above_at_and_below_the_catalog_root() {
        let catalog = Path::new("vendor/library/skills");
        assert!(gitlink_intersects_catalog(
            Path::new("vendor/library"),
            catalog
        ));
        assert!(gitlink_intersects_catalog(catalog, catalog));
        assert!(gitlink_intersects_catalog(
            Path::new("vendor/library/skills/demo"),
            catalog
        ));
        assert!(gitlink_intersects_catalog(
            Path::new("vendor/library"),
            Path::new(".")
        ));
        assert!(!gitlink_intersects_catalog(
            Path::new("vendor/other"),
            catalog
        ));
    }
}

impl RepositoryUpdateVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UpToDate => "up_to_date",
            Self::Ahead => "ahead",
            Self::Available => "available",
            Self::Blocked => "blocked",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "up_to_date" => Some(Self::UpToDate),
            "ahead" => Some(Self::Ahead),
            "available" => Some(Self::Available),
            "blocked" => Some(Self::Blocked),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CachedUpdateCheck {
    pub source_id: i64,
    pub checked_at: i64,
    pub local_revision: String,
    pub local_reference: Option<String>,
    pub upstream_ref: Option<String>,
    pub upstream_revision: Option<String>,
    pub merge_base: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub dirty: bool,
    pub dirty_known: bool,
    pub verdict: RepositoryUpdateVerdict,
    pub detail: String,
}

impl CachedUpdateCheck {
    pub fn superseded_by(&self, source: &RegisteredSource) -> bool {
        let findings = self.findings();
        let observed_missing = findings
            .iter()
            .any(|finding| finding.code() == "source.missing");
        // A check that refused before reading HEAD holds no reference, which is
        // not the same observation as a detached HEAD. Comparing the absence
        // against a branch the source reports would supersede such a check the
        // moment it was taken, and Doctor would never list the refusal that a
        // fresh check would only repeat.
        let observed_reference = !findings.iter().any(|finding| {
            matches!(
                finding.code(),
                "source.missing" | "source.partial_clone_unsupported"
            )
        });
        let reference_changed = match (self.local_reference.as_deref(), source.branch()) {
            (Some(reference), Some(branch)) => !Self::reference_names_branch(reference, branch),
            (None, None) => false,
            _ => true,
        };
        (source.source_error().is_some() && !observed_missing)
            || (source.source_error().is_none() && observed_missing)
            || (observed_reference && reference_changed)
            || self.local_revision != source.head()
            || (self.dirty_known && source.dirty().is_some_and(|dirty| dirty != self.dirty))
    }
    /// Whether a recorded head reference is the branch a registered source
    /// records, over every spelling Git can produce for it.
    ///
    /// A check records the full reference `git symbolic-ref HEAD` prints; a
    /// source records what `--short` printed for the same HEAD, which is the
    /// shortest suffix that still resolves back to it. Normally that is `main`,
    /// but a tag of the same name makes the bare name ambiguous and Git prints
    /// `heads/main` instead — and `refs/heads/main` when even that is taken.
    /// Rebuilding the reference from the short name would spell one of those
    /// `refs/heads/heads/main`, and a repository whose branch shares a name
    /// with a tag would then supersede every check it ever ran.
    fn reference_names_branch(reference: &str, branch: &str) -> bool {
        reference == branch
            || reference == format!("refs/{branch}")
            || reference == format!("refs/heads/{branch}")
    }
    pub fn availability_known(&self) -> bool {
        !self.findings().iter().any(|finding| {
            matches!(
                finding.code(),
                "source.fetch_failed"
                    | "source.partial_clone_unsupported"
                    | "source.submodule_update_unsupported"
                    | "source.changed_after_preview"
                    | "source.no_upstream"
                    | "source.missing"
                    | "source.detached_head"
                    | "update.verification_failed"
                    | "update.verification_incomplete"
            )
        })
    }
    pub fn finding(&self) -> Option<Finding> {
        self.findings().into_iter().next()
    }
    pub fn findings(&self) -> Vec<Finding> {
        if self.verdict != RepositoryUpdateVerdict::Blocked {
            return Vec::new();
        }
        decode_findings(&self.detail)
    }
}

pub(crate) fn cached_update_check(
    source: &RegisteredSource,
    probe: &RepositoryUpdateProbe,
    checked_at: i64,
) -> CachedUpdateCheck {
    let (mut verdict, mut findings) = classify_repository_update(probe);
    if let Some(finding) = incoming_collision_finding(probe) {
        findings.push(finding);
        verdict = RepositoryUpdateVerdict::Blocked;
    }
    if let Some(finding) = changed_submodule_catalog_finding(source, probe) {
        findings.push(finding);
        verdict = RepositoryUpdateVerdict::Blocked;
    }
    let detail = encode_findings(&findings);
    let observed_dirty = probe.worktree.as_ref().and_then(|state| {
        if state.tracked_dirty() || !state.untracked.is_empty() {
            Some(true)
        } else {
            state.worktree_dirty_known.then_some(false)
        }
    });
    // A check that read the worktree and could not tell whether it is dirty —
    // a configured filter driver makes ` M` ambiguous, and a cancellable check
    // will not run the second Git process that would settle it — records that
    // it does not know. Borrowing the registered source's last value instead
    // would state a cleanliness this check never observed, and the very next
    // read of the source would then supersede the check for disagreeing with
    // it, taking the blocked finding out of Doctor on the way.
    let dirtiness_withheld = probe.worktree.is_some() && observed_dirty.is_none();
    CachedUpdateCheck {
        source_id: source.id(),
        checked_at,
        local_revision: probe
            .local
            .as_ref()
            .map(HeadState::revision)
            .unwrap_or(source.head())
            .to_owned(),
        local_reference: probe
            .local
            .as_ref()
            .and_then(HeadState::reference)
            .map(str::to_owned),
        upstream_ref: probe
            .upstream
            .as_ref()
            .map(|upstream| upstream.tracking_ref().to_owned()),
        upstream_revision: probe
            .upstream
            .as_ref()
            .map(|upstream| upstream.revision().to_owned()),
        merge_base: probe.merge_base.clone(),
        ahead: probe.ahead,
        behind: probe.behind,
        dirty: !dirtiness_withheld && observed_dirty.or_else(|| source.dirty()).unwrap_or(false),
        dirty_known: !dirtiness_withheld && (observed_dirty.is_some() || source.dirty().is_some()),
        verdict,
        detail,
    }
}

const FINDINGS_DETAIL_PREFIX: &str = "findings-v1;";

fn encode_findings(findings: &[Finding]) -> String {
    if findings.is_empty() {
        return String::new();
    }
    let mut encoded = FINDINGS_DETAIL_PREFIX.to_owned();
    for finding in findings {
        let code = finding.code();
        let evidence = finding.evidence();
        encoded.push_str(&format!("{}:{}:", code.len(), evidence.len()));
        encoded.push_str(code);
        encoded.push_str(evidence);
    }
    encoded
}

fn decode_findings(detail: &str) -> Vec<Finding> {
    let Some(mut remaining) = detail.strip_prefix(FINDINGS_DETAIL_PREFIX) else {
        if detail.is_empty() {
            return Vec::new();
        }
        let (code, evidence) = detail
            .split_once('|')
            .unwrap_or(("source.fetch_failed", detail));
        return vec![finding_from_parts(code, evidence)];
    };
    let mut findings = Vec::new();
    while !remaining.is_empty() {
        let Some((code_len, after_code_len)) = remaining.split_once(':') else {
            return vec![finding_from_parts("source.fetch_failed", detail)];
        };
        let Some((evidence_len, payload)) = after_code_len.split_once(':') else {
            return vec![finding_from_parts("source.fetch_failed", detail)];
        };
        let (Ok(code_len), Ok(evidence_len)) =
            (code_len.parse::<usize>(), evidence_len.parse::<usize>())
        else {
            return vec![finding_from_parts("source.fetch_failed", detail)];
        };
        let Some(total_len) = code_len.checked_add(evidence_len) else {
            return vec![finding_from_parts("source.fetch_failed", detail)];
        };
        if payload.len() < total_len
            || !payload.is_char_boundary(code_len)
            || !payload.is_char_boundary(total_len)
        {
            return vec![finding_from_parts("source.fetch_failed", detail)];
        }
        let (code, rest) = payload.split_at(code_len);
        let (evidence, next) = rest.split_at(evidence_len);
        findings.push(finding_from_parts(code, evidence));
        remaining = next;
    }
    findings
}

fn finding_from_parts(code: &str, evidence: &str) -> Finding {
    Finding::new(
        leak_code(code),
        if code == "update.verification_failed" {
            FindingSeverity::Critical
        } else {
            FindingSeverity::Warning
        },
        evidence.to_owned(),
    )
}

fn leak_code(code: &str) -> &'static str {
    match code {
        "source.dirty" => "source.dirty",
        "source.diverged" => "source.diverged",
        "source.missing" => "source.missing",
        "source.detached_head" => "source.detached_head",
        "source.no_upstream" => "source.no_upstream",
        "source.fetch_failed" => "source.fetch_failed",
        "source.partial_clone_unsupported" => "source.partial_clone_unsupported",
        "source.submodule_update_unsupported" => "source.submodule_update_unsupported",
        "source.changed_after_preview" => "source.changed_after_preview",
        "update.verification_failed" => "update.verification_failed",
        "update.verification_incomplete" => "update.verification_incomplete",
        _ => "source.fetch_failed",
    }
}

#[derive(Clone, Debug)]
pub struct RepositoryUpdateProbe {
    pub path: PathBuf,
    pub local: Option<HeadState>,
    pub upstream: Option<Upstream>,
    pub merge_base: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub worktree: Option<WorktreeState>,
    pub changed_files: Vec<ChangedPath>,
    pub error: Option<String>,
}

pub fn probe_repository_update(source: &RegisteredSource, fetch: bool) -> RepositoryUpdateProbe {
    let path = source.git_top_level().to_path_buf();
    if let Some(error) = source.source_error() {
        return RepositoryUpdateProbe {
            path,
            local: None,
            upstream: None,
            merge_base: None,
            ahead: 0,
            behind: 0,
            worktree: None,
            changed_files: Vec::new(),
            error: Some(format!("source.missing|{error}")),
        };
    }
    if !registered_checkout_path_is_current(source) {
        return changed_checkout_probe(path);
    }
    match probe_existing(&path, fetch) {
        Ok(probe) => probe,
        Err(error) => failed_probe(path, error),
    }
}

pub(crate) fn probe_repository_update_cancellable(
    source: &RegisteredSource,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Option<RepositoryUpdateProbe> {
    let path = source.git_top_level().to_path_buf();
    if let Some(error) = source.source_error() {
        return Some(RepositoryUpdateProbe {
            path,
            local: None,
            upstream: None,
            merge_base: None,
            ahead: 0,
            behind: 0,
            worktree: None,
            changed_files: Vec::new(),
            error: Some(format!("source.missing|{error}")),
        });
    }
    let checkout_is_current =
        registered_checkout_path_is_current_cancellable(source, cancelled, child_slot)?;
    if !checkout_is_current {
        return Some(changed_checkout_probe(path));
    }
    match probe_existing_cancellable(&path, cancelled, child_slot) {
        Ok(Some(probe)) => Some(probe),
        Ok(None) => None,
        Err(error) => Some(failed_probe_without_reinspection(path, error)),
    }
}

fn probe_existing_cancellable(
    path: &Path,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> std::result::Result<Option<RepositoryUpdateProbe>, ProbeFailure> {
    let Some(partial_clone) =
        git::repository_is_partial_clone_cancellable(path, cancelled, child_slot)
            .map_err(ProbeFailure::Inspect)?
    else {
        return Ok(None);
    };
    if partial_clone {
        return Ok(Some(partial_clone_probe(path)));
    }
    let Some(local) =
        git::head_state_cancellable(path, cancelled, child_slot).map_err(ProbeFailure::Inspect)?
    else {
        return Ok(None);
    };
    let Some(worktree) = git::worktree_state_cancellable(path, cancelled, child_slot)
        .map_err(ProbeFailure::Inspect)?
    else {
        return Ok(None);
    };
    let Some(upstream) = git::upstream_of_cancellable(path, &local, cancelled, child_slot)
        .map_err(ProbeFailure::Inspect)?
    else {
        return Ok(None);
    };
    let Some(upstream) = upstream else {
        return Ok(Some(no_upstream_probe(path, local, worktree)));
    };
    let revision = if upstream.remote() == "." {
        upstream.revision().to_owned()
    } else {
        match git::fetch_upstream_cancellable(path, &upstream, cancelled, child_slot) {
            Ok(Some(revision)) => revision,
            Ok(None) => return Ok(None),
            Err(error) => {
                return Ok(Some(RepositoryUpdateProbe {
                    path: path.into(),
                    local: Some(local),
                    upstream: Some(upstream),
                    merge_base: None,
                    ahead: 0,
                    behind: 0,
                    worktree: Some(worktree),
                    changed_files: Vec::new(),
                    error: Some(format!("source.fetch_failed|{error}")),
                }));
            }
        }
    };
    let upstream = upstream.with_revision(revision);
    let Some(base) = git::merge_base_cancellable(
        path,
        local.revision(),
        upstream.revision(),
        cancelled,
        child_slot,
    )
    .map_err(ProbeFailure::Inspect)?
    else {
        return Ok(None);
    };
    let Some(counts) = git::ahead_behind_cancellable(
        path,
        local.revision(),
        upstream.revision(),
        cancelled,
        child_slot,
    )
    .map_err(ProbeFailure::Inspect)?
    else {
        return Ok(None);
    };
    let changed_files = if counts.behind > 0 {
        let Some(files) = git::changed_paths_cancellable(
            path,
            local.revision(),
            upstream.revision(),
            cancelled,
            child_slot,
        )
        .map_err(ProbeFailure::Inspect)?
        else {
            return Ok(None);
        };
        files
    } else {
        Vec::new()
    };
    Ok(Some(RepositoryUpdateProbe {
        path: path.into(),
        local: Some(local),
        upstream: Some(upstream),
        merge_base: base,
        ahead: counts.ahead,
        behind: counts.behind,
        worktree: Some(worktree),
        changed_files,
        error: None,
    }))
}

/// Re-read local preconditions while retaining the exact upstream object an
/// earlier explicit check fetched. This performs no network access and refuses
/// the cached object if the branch's configured tracking ref changed.
pub fn probe_repository_update_against(
    source: &RegisteredSource,
    upstream_ref: &str,
    upstream_revision: &str,
) -> RepositoryUpdateProbe {
    let path = source.git_top_level().to_path_buf();
    if let Some(error) = source.source_error() {
        return RepositoryUpdateProbe {
            path,
            local: None,
            upstream: None,
            merge_base: None,
            ahead: 0,
            behind: 0,
            worktree: None,
            changed_files: Vec::new(),
            error: Some(format!("source.missing|{error}")),
        };
    }
    if !registered_checkout_path_is_current(source) {
        return changed_checkout_probe(path);
    }
    match probe_existing_against(&path, upstream_ref, upstream_revision) {
        Ok(probe) => probe,
        Err(crate::Error::SourceChangedAfterPreview) => changed_after_preview_probe(path),
        Err(error) => failed_probe(path, ProbeFailure::Inspect(error)),
    }
}

enum ProbeFailure {
    Fetch(crate::Error),
    Inspect(crate::Error),
}

fn failed_probe(path: PathBuf, error: ProbeFailure) -> RepositoryUpdateProbe {
    let local = git::head_state(&path).ok();
    let worktree = git::worktree_state(&path).ok();
    let (code, error) = match error {
        ProbeFailure::Fetch(error) => ("source.fetch_failed", error),
        ProbeFailure::Inspect(error) => ("source.missing", error),
    };
    RepositoryUpdateProbe {
        path,
        local,
        upstream: None,
        merge_base: None,
        ahead: 0,
        behind: 0,
        worktree,
        changed_files: Vec::new(),
        error: Some(format!("{code}|{error}")),
    }
}

fn failed_probe_without_reinspection(path: PathBuf, error: ProbeFailure) -> RepositoryUpdateProbe {
    let (code, error) = match error {
        ProbeFailure::Fetch(error) => ("source.fetch_failed", error),
        ProbeFailure::Inspect(error) => ("source.missing", error),
    };
    RepositoryUpdateProbe {
        path,
        local: None,
        upstream: None,
        merge_base: None,
        ahead: 0,
        behind: 0,
        worktree: None,
        changed_files: Vec::new(),
        error: Some(format!("{code}|{error}")),
    }
}

fn changed_after_preview_probe(path: PathBuf) -> RepositoryUpdateProbe {
    RepositoryUpdateProbe {
        path,
        local: None,
        upstream: None,
        merge_base: None,
        ahead: 0,
        behind: 0,
        worktree: None,
        changed_files: Vec::new(),
        error: Some(
            "source.changed_after_preview|the upstream state changed after the explicit check"
                .into(),
        ),
    }
}

fn changed_checkout_probe(path: PathBuf) -> RepositoryUpdateProbe {
    RepositoryUpdateProbe {
        path,
        local: None,
        upstream: None,
        merge_base: None,
        ahead: 0,
        behind: 0,
        worktree: None,
        changed_files: Vec::new(),
        error: Some(
            "source.missing|the registered checkout changed or was replaced since it was inspected"
                .into(),
        ),
    }
}

fn partial_clone_probe(path: &Path) -> RepositoryUpdateProbe {
    RepositoryUpdateProbe {
        path: path.into(),
        local: None,
        upstream: None,
        merge_base: None,
        ahead: 0,
        behind: 0,
        worktree: None,
        changed_files: Vec::new(),
        error: Some(
            "source.partial_clone_unsupported|partial-clone repositories are not supported because Git may fetch missing objects outside the explicit check"
                .into(),
        ),
    }
}

fn registered_checkout_path_is_current(source: &RegisteredSource) -> bool {
    let path = source.git_top_level();
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return false;
    }
    if path.canonicalize().ok().as_deref() != Some(path) {
        return false;
    }
    source
        .repository_identity()
        .is_some_and(|expected| repository_identity(path).is_ok_and(|current| current == *expected))
}

fn registered_checkout_path_is_current_cancellable(
    source: &RegisteredSource,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Option<bool> {
    if cancelled.load(std::sync::atomic::Ordering::Acquire) {
        return None;
    }
    let path = source.git_top_level();
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return Some(false);
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || path.canonicalize().ok().as_deref() != Some(path)
    {
        return Some(false);
    }
    let git_dir = match git::repository_git_dir_cancellable(path, cancelled, child_slot) {
        Ok(Some(path)) => path,
        Ok(None) => return None,
        Err(_) => return Some(false),
    };
    Some(source.repository_identity().is_some_and(|expected| {
        repository_identity_from_git_dir(git_dir).is_ok_and(|current| current == *expected)
    }))
}

fn probe_existing_against(
    path: &Path,
    expected_upstream_ref: &str,
    upstream_revision: &str,
) -> Result<RepositoryUpdateProbe> {
    let local = git::head_state(path)?;
    let worktree = git::worktree_state(path)?;
    let Some(upstream) = git::upstream_of(path, &local)? else {
        return Ok(no_upstream_probe(path, local, worktree));
    };
    if upstream.tracking_ref() != expected_upstream_ref {
        return Ok(no_upstream_probe(path, local, worktree));
    }
    if upstream.revision() != upstream_revision {
        return Err(crate::Error::SourceChangedAfterPreview);
    }
    finish_probe(
        path,
        local,
        worktree,
        upstream.with_revision(upstream_revision.to_owned()),
    )
}

fn probe_existing(
    path: &Path,
    fetch: bool,
) -> std::result::Result<RepositoryUpdateProbe, ProbeFailure> {
    if git::repository_is_partial_clone(path).map_err(ProbeFailure::Inspect)? {
        return Ok(partial_clone_probe(path));
    }
    let local = git::head_state(path).map_err(ProbeFailure::Inspect)?;
    let worktree = git::worktree_state(path).map_err(ProbeFailure::Inspect)?;
    let mut upstream = git::upstream_of(path, &local).map_err(ProbeFailure::Inspect)?;
    if fetch
        && let Some(value) = &upstream
        && value.remote() != "."
    {
        let revision = git::fetch_upstream(path, value).map_err(ProbeFailure::Fetch)?;
        upstream = Some(value.with_revision(revision));
    }
    let Some(upstream_value) = upstream else {
        return Ok(no_upstream_probe(path, local, worktree));
    };
    finish_probe(path, local, worktree, upstream_value).map_err(ProbeFailure::Inspect)
}

fn no_upstream_probe(
    path: &Path,
    local: HeadState,
    worktree: WorktreeState,
) -> RepositoryUpdateProbe {
    RepositoryUpdateProbe {
        path: path.into(),
        local: Some(local),
        upstream: None,
        merge_base: None,
        ahead: 0,
        behind: 0,
        worktree: Some(worktree),
        changed_files: Vec::new(),
        error: Some("source.no_upstream|no upstream is configured for this branch".into()),
    }
}

fn finish_probe(
    path: &Path,
    local: HeadState,
    worktree: WorktreeState,
    upstream: Upstream,
) -> Result<RepositoryUpdateProbe> {
    let base = git::merge_base(path, local.revision(), upstream.revision())?;
    let counts = git::ahead_behind(path, local.revision(), upstream.revision())?;
    let changed_files = if counts.behind > 0 {
        git::changed_paths(path, local.revision(), upstream.revision())?
    } else {
        Vec::new()
    };
    Ok(RepositoryUpdateProbe {
        path: path.into(),
        local: Some(local),
        upstream: Some(upstream),
        merge_base: base,
        ahead: counts.ahead,
        behind: counts.behind,
        worktree: Some(worktree),
        changed_files,
        error: None,
    })
}

fn incoming_collision_finding(probe: &RepositoryUpdateProbe) -> Option<Finding> {
    let path = incoming_untracked_collision(probe.worktree.as_ref()?, &probe.changed_files)?;
    Some(Finding::new(
        "source.dirty",
        FindingSeverity::Warning,
        format!(
            "untracked path {} conflicts with an incoming path",
            path.display()
        ),
    ))
}

fn changed_submodule_catalog_finding(
    source: &RegisteredSource,
    probe: &RepositoryUpdateProbe,
) -> Option<Finding> {
    let changed = probe
        .changed_files
        .iter()
        .find(|changed| changed.is_gitlink())?;
    let catalog = source
        .catalogs()
        .iter()
        .filter(|catalog| catalog.included())
        .map(|catalog| catalog.relative_path())
        .find(|catalog| {
            gitlink_intersects_catalog(changed.path(), catalog)
                || changed
                    .renamed_from()
                    .is_some_and(|old| gitlink_intersects_catalog(old, catalog))
        });
    let endpoint = changed.renamed_from().unwrap_or_else(|| changed.path());
    let evidence = catalog.map_or_else(
        || {
            format!(
                "the update changes submodule {}; Skilled does not update or verify submodule worktrees",
                endpoint.display()
            )
        },
        |catalog| {
            format!(
                "the update changes submodule {} intersecting registered catalog {}; Skilled does not update or verify submodule worktrees",
                endpoint.display(),
                catalog.display()
            )
        },
    );
    Some(Finding::new(
        "source.submodule_update_unsupported",
        FindingSeverity::Warning,
        evidence,
    ))
}

fn gitlink_intersects_catalog(gitlink: &Path, catalog: &Path) -> bool {
    catalog == Path::new(".") || catalog.starts_with(gitlink) || gitlink.starts_with(catalog)
}

pub fn classify_repository_update(
    probe: &RepositoryUpdateProbe,
) -> (RepositoryUpdateVerdict, Vec<Finding>) {
    let mut findings = Vec::new();
    if let Some(error) = &probe.error {
        let (code, evidence) = error
            .split_once('|')
            .unwrap_or(("source.fetch_failed", error.as_str()));
        findings.push(Finding::new(
            leak_code(code),
            FindingSeverity::Warning,
            evidence.to_owned(),
        ));
    }
    if matches!(probe.local, Some(HeadState::Detached { .. })) {
        findings.push(Finding::new(
            "source.detached_head",
            FindingSeverity::Warning,
            "HEAD is detached".into(),
        ));
    }
    if let Some(state) = &probe.worktree
        && (state.tracked_dirty() || !state.worktree_dirty_known)
    {
        let evidence = if !state.worktree_dirty_known && !state.tracked_dirty() {
            "worktree cleanliness could not be determined without running configured filters"
        } else {
            match (state.index_dirty, state.worktree_dirty) {
                (true, true) => "the index and worktree contain tracked changes",
                (true, false) => "the index contains staged changes",
                _ => "the worktree contains tracked changes",
            }
        };
        findings.push(Finding::new(
            "source.dirty",
            FindingSeverity::Warning,
            evidence.into(),
        ));
    }
    if probe.ahead > 0 && probe.behind > 0 {
        findings.push(Finding::new(
            "source.diverged",
            FindingSeverity::Warning,
            format!(
                "local and upstream diverged ({} ahead, {} behind)",
                probe.ahead, probe.behind
            ),
        ));
    }
    let verdict = if !findings.is_empty() {
        RepositoryUpdateVerdict::Blocked
    } else if probe.behind > 0 {
        RepositoryUpdateVerdict::Available
    } else if probe.ahead > 0 {
        RepositoryUpdateVerdict::Ahead
    } else {
        RepositoryUpdateVerdict::UpToDate
    };
    (verdict, findings)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AffectedInstallations {
    pub updated: Vec<String>,
    pub removed: Vec<String>,
    pub added: Vec<String>,
    pub renamed: Vec<(String, String)>,
    pub complete: bool,
    pub incomplete_reason: Option<String>,
    expected_dangling: Vec<(String, usize)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryUpdatePlan {
    source_id: i64,
    source_label: String,
    path: PathBuf,
    repository_identity: RepositoryIdentity,
    current_reference: String,
    current_revision: String,
    target_revision: String,
    upstream_ref: String,
    commits: Vec<String>,
    changed_files: Vec<ChangedPath>,
    affected: AffectedInstallations,
    findings: Vec<Finding>,
    hooks_disclosure: String,
}

impl RepositoryUpdatePlan {
    pub fn source_id(&self) -> i64 {
        self.source_id
    }
    pub fn source_label(&self) -> &str {
        &self.source_label
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn current_revision(&self) -> &str {
        &self.current_revision
    }
    pub fn current_reference(&self) -> &str {
        &self.current_reference
    }
    pub fn target_revision(&self) -> &str {
        &self.target_revision
    }
    pub fn upstream_ref(&self) -> &str {
        &self.upstream_ref
    }
    pub fn commits(&self) -> &[String] {
        &self.commits
    }
    pub fn changed_files(&self) -> &[ChangedPath] {
        &self.changed_files
    }
    pub fn affected(&self) -> &AffectedInstallations {
        &self.affected
    }
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }
    pub fn hooks_disclosure(&self) -> &str {
        &self.hooks_disclosure
    }
    pub fn is_blocked(&self) -> bool {
        !self.findings.is_empty()
    }
}

pub fn plan_repository_update(
    source: &RegisteredSource,
    probe: &RepositoryUpdateProbe,
    inventory: &InventorySnapshot,
) -> Result<RepositoryUpdatePlan> {
    let (verdict, mut findings) = classify_repository_update(probe);
    let local = probe
        .local
        .as_ref()
        .map(HeadState::revision)
        .unwrap_or(source.head())
        .to_owned();
    let target = if verdict == RepositoryUpdateVerdict::Available {
        probe
            .upstream
            .as_ref()
            .map(Upstream::revision)
            .unwrap_or(&local)
    } else {
        &local
    }
    .to_owned();
    let upstream_ref = probe
        .upstream
        .as_ref()
        .map(Upstream::tracking_ref)
        .unwrap_or("")
        .to_owned();
    let changed_files = if verdict == RepositoryUpdateVerdict::Available {
        probe.changed_files.clone()
    } else {
        Vec::new()
    };
    let commits = if verdict == RepositoryUpdateVerdict::Available {
        git::commit_summaries(&probe.path, &local, &target)?
    } else {
        Vec::new()
    };
    if let Some(finding) = incoming_collision_finding(probe) {
        findings.push(finding);
    }
    if let Some(finding) = changed_submodule_catalog_finding(source, probe) {
        findings.push(finding);
    }
    let affected = affected_catalog_skills(
        source,
        inventory,
        &changed_files,
        &probe.path,
        &local,
        &target,
    )?;
    Ok(RepositoryUpdatePlan {
        source_id: source.id(),
        source_label: source.label().into(),
        path: probe.path.clone(),
        repository_identity: source
            .repository_identity()
            .cloned()
            .ok_or(crate::Error::SourceChangedAfterPreview)?,
        current_reference: probe
            .local
            .as_ref()
            .and_then(HeadState::reference)
            .unwrap_or("")
            .to_owned(),
        current_revision: local,
        target_revision: target,
        upstream_ref,
        commits,
        changed_files,
        affected,
        findings,
        // Filters belong beside the hooks. `git::fast_forward` deliberately
        // hands Git the repository's own configuration, so materialising the
        // updated paths can run a configured smudge or process filter — local
        // commands, and in the case of Git LFS further network access. The
        // check suppresses repository code precisely because it was not agreed
        // to; what the fast-forward runs is agreed to, which requires saying so.
        hooks_disclosure: "Git may run your post-merge and reference-transaction hooks, and any \
             checkout filters this repository configures, during this fast-forward."
            .into(),
    })
}

fn incoming_untracked_collision<'a>(
    worktree: &'a WorktreeState,
    files: &[ChangedPath],
) -> Option<&'a Path> {
    worktree
        .untracked
        .iter()
        .chain(&worktree.ignored)
        .map(PathBuf::as_path)
        .find(|untracked| {
            files
                .iter()
                .filter(|file| !matches!(file.kind(), ChangeKind::Deleted))
                .map(ChangedPath::path)
                .any(|incoming| {
                    incoming == *untracked
                        || incoming.starts_with(untracked)
                        || untracked.starts_with(incoming)
                })
        })
}

fn affected_catalog_skills(
    source: &RegisteredSource,
    inventory: &InventorySnapshot,
    files: &[ChangedPath],
    repository: &Path,
    current_revision: &str,
    target_revision: &str,
) -> Result<AffectedInstallations> {
    #[derive(Clone, Default)]
    struct Changes {
        catalog_path: PathBuf,
        candidate_path: PathBuf,
        added: bool,
        deleted: bool,
        changed: bool,
        added_suffixes: std::collections::BTreeSet<PathBuf>,
        deleted_suffixes: std::collections::BTreeSet<PathBuf>,
    }

    let mut changes = std::collections::BTreeMap::<(PathBuf, String), Changes>::new();
    let mut rename_evidence = std::collections::BTreeMap::<
        (PathBuf, String, String),
        std::collections::BTreeSet<(PathBuf, PathBuf)>,
    >::new();
    for file in files {
        let path_changes = if let Some(old_path) = file.renamed_from() {
            vec![
                (old_path.to_path_buf(), ChangeKind::Deleted),
                (file.path().to_path_buf(), ChangeKind::Added),
            ]
        } else {
            vec![(file.path().to_path_buf(), file.kind())]
        };
        for catalog in source
            .catalogs()
            .iter()
            .filter(|catalog| catalog.included())
        {
            for (path, kind) in &path_changes {
                let Some((name, candidate_path, suffix)) =
                    catalog_candidate_for_change(catalog, path)
                else {
                    continue;
                };
                let entry = changes
                    .entry((catalog.relative_path().to_path_buf(), name))
                    .or_default();
                entry.catalog_path = catalog.relative_path().to_path_buf();
                entry.candidate_path = candidate_path;
                match kind {
                    ChangeKind::Added => {
                        entry.added = true;
                        entry.added_suffixes.insert(suffix);
                    }
                    ChangeKind::Deleted => {
                        entry.deleted = true;
                        entry.deleted_suffixes.insert(suffix);
                    }
                    _ => entry.changed = true,
                }
            }
            if let Some(old_path) = file.renamed_from()
                && let (Some((old_name, _, old_suffix)), Some((new_name, _, new_suffix))) = (
                    catalog_candidate_for_change(catalog, old_path),
                    catalog_candidate_for_change(catalog, file.path()),
                )
                && old_name != new_name
            {
                rename_evidence
                    .entry((catalog.relative_path().to_path_buf(), old_name, new_name))
                    .or_default()
                    .insert((old_suffix, new_suffix));
            }
        }
    }

    // A catalog whose skill is its root is the repository itself: it exists at
    // every revision, so nothing can add it, move it away, or leave it behind.
    let candidate_exists = |revision: &str, path: &Path| -> Result<bool> {
        if path == Path::new(".") {
            return Ok(true);
        }
        git::tree_directory_exists(repository, revision, path)
    };

    // A rename is stated only when Git proved every path in both changed
    // directories to be an exact-content rename. Partial or edited moves stay
    // the conservative removed-plus-added pair.
    let mut rename_candidates = Vec::new();
    for ((catalog_path, old_name, new_name), pairs) in &rename_evidence {
        let old_key = (catalog_path.clone(), old_name.clone());
        let new_key = (catalog_path.clone(), new_name.clone());
        let Some(old_change) = changes.get(&old_key) else {
            continue;
        };
        let Some(new_change) = changes.get(&new_key) else {
            continue;
        };
        let old_suffixes = pairs
            .iter()
            .map(|(old, _)| old)
            .collect::<std::collections::BTreeSet<_>>();
        let new_suffixes = pairs
            .iter()
            .map(|(_, new)| new)
            .collect::<std::collections::BTreeSet<_>>();
        if !old_change.added
            && !old_change.changed
            && !new_change.deleted
            && !new_change.changed
            && old_change
                .deleted_suffixes
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                == old_suffixes
            && new_change
                .added_suffixes
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                == new_suffixes
            // Matching every changed path does not make the move whole: what
            // did not change stayed where it was. Moving one file out of a
            // skill that keeps its SKILL.md would otherwise be stated as a
            // rename, and verification would then expect a link to dangle for
            // a skill that is still there.
            && !candidate_exists(target_revision, &old_change.candidate_path)?
            && !candidate_exists(current_revision, &new_change.candidate_path)?
        {
            rename_candidates.push((old_key, new_key));
        }
    }
    let mut old_counts = std::collections::BTreeMap::new();
    let mut new_counts = std::collections::BTreeMap::new();
    for (old, new) in &rename_candidates {
        *old_counts.entry(old.clone()).or_insert(0_usize) += 1;
        *new_counts.entry(new.clone()).or_insert(0_usize) += 1;
    }
    let confirmed_renames = rename_candidates
        .into_iter()
        .filter(|(old, new)| old_counts.get(old) == Some(&1) && new_counts.get(new) == Some(&1))
        .collect::<Vec<_>>();

    let mut installed =
        std::collections::BTreeMap::<(PathBuf, String), std::collections::BTreeSet<usize>>::new();
    for row in inventory.rows() {
        for observation in row.observations() {
            let Some(variant) = observation
                .resolution()
                .filter(|variant| variant.source_id() == source.id())
            else {
                continue;
            };
            installed
                .entry((
                    variant.catalog_relative_path().to_path_buf(),
                    row.name().to_owned(),
                ))
                .or_default()
                .insert(observation.agent().index());
        }
    }
    let mut updated = Vec::new();
    let mut removed_changes = Vec::new();
    let mut added_changes = Vec::new();
    for ((catalog_path, name), change) in &changes {
        let key = (catalog_path.clone(), name.clone());
        if confirmed_renames
            .iter()
            .any(|(old, new)| old == &key || new == &key)
        {
            continue;
        }
        let target_keeps_skill = candidate_exists(target_revision, &change.candidate_path)?;
        // A file added inside a skill that was already there does not make the
        // skill new. Only a candidate the current revision does not have can be
        // stated as added upstream; a catalog whose skill is its root always
        // has one, so nothing about it is ever reported that way.
        let skill_is_new = !candidate_exists(current_revision, &change.candidate_path)?;
        let installed_agents = installed.get(&(catalog_path.clone(), name.clone()));
        if installed_agents.is_none() {
            if change.added && target_keeps_skill && skill_is_new {
                added_changes.push((name.clone(), change.clone()));
            }
            continue;
        }
        if !target_keeps_skill {
            removed_changes.push((
                name.clone(),
                change.clone(),
                installed_agents.cloned().unwrap_or_default(),
            ));
        } else {
            updated.push(name.clone());
        }
    }

    let mut expected_dangling = Vec::new();
    let removed = removed_changes
        .into_iter()
        .map(|(name, _, agents)| {
            expected_dangling.extend(agents.into_iter().map(|agent| (name.clone(), agent)));
            name
        })
        .collect();
    let mut added = added_changes
        .into_iter()
        .map(|(name, _)| name)
        .collect::<Vec<_>>();
    updated.sort();
    updated.dedup();
    let mut removed: Vec<String> = removed;
    removed.sort();
    removed.dedup();
    added.sort();
    added.dedup();
    let mut renamed = Vec::new();
    for ((catalog_path, old), (_, new)) in confirmed_renames {
        let Some(agents) = installed.get(&(catalog_path, old.clone())) else {
            continue;
        };
        expected_dangling.extend(agents.iter().map(|agent| (old.clone(), *agent)));
        renamed.push((old, new));
    }
    renamed.sort();
    renamed.dedup();
    expected_dangling.sort();
    expected_dangling.dedup();
    Ok(AffectedInstallations {
        updated,
        removed,
        added,
        renamed,
        complete: inventory.counts_are_complete() && inventory.registry_is_complete(),
        incomplete_reason: if inventory.counts_are_complete() && inventory.registry_is_complete() {
            None
        } else if inventory.scan_pending() {
            Some("installation inventory has not been scanned; setup may not be complete".into())
        } else if inventory.no_agent_configured() {
            Some("no agent is configured for installation scanning".into())
        } else {
            Some("one or more installation roots could not be fully read".into())
        },
        expected_dangling,
    })
}

fn catalog_candidate_for_change(
    catalog: &CatalogProposal,
    path: &Path,
) -> Option<(String, PathBuf, PathBuf)> {
    let catalog_path = catalog.relative_path();
    let relative = if catalog_path == Path::new(".") {
        path
    } else {
        path.strip_prefix(catalog_path).ok()?
    };
    if catalog_path == Path::new(".") {
        let candidate = catalog.candidates().first()?;
        Some((
            candidate.directory_name().to_owned(),
            PathBuf::from("."),
            relative.to_path_buf(),
        ))
    } else {
        let mut components = relative.components();
        let name = components.next()?.as_os_str().to_str()?;
        Some((
            name.to_owned(),
            catalog_path.join(name),
            components.as_path().to_path_buf(),
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryVerifyReport {
    verified: bool,
    complete: bool,
    failures: Vec<String>,
    withheld: Vec<String>,
}
impl RepositoryVerifyReport {
    pub fn is_verified(&self) -> bool {
        self.verified
    }
    pub fn is_complete(&self) -> bool {
        self.complete
    }
    pub fn failures(&self) -> &[String] {
        &self.failures
    }
    pub fn withheld(&self) -> &[String] {
        &self.withheld
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RepositoryUpdatePrompt {
    Preview(RepositoryUpdatePlan),
    Report {
        plan: RepositoryUpdatePlan,
        verification: RepositoryVerifyReport,
        apply_error: Option<String>,
        persistence_error: Option<String>,
    },
    Failed(String),
}

pub fn apply_repository_update(plan: &RepositoryUpdatePlan) -> Result<()> {
    validate_repository_update(plan)?;
    git::fast_forward(&plan.path, &plan.target_revision)
}

/// Apply while reporting whether Git's write operation was reached. Callers
/// use the distinction to rescan after every refusal without turning a
/// pre-write guard into a failed postcondition.
///
/// The guard and the write are two Git invocations, and `merge --ff-only`
/// takes no expected-current-revision to condition its write on, so another
/// process that moves the branch in between can leave the fast-forward
/// applying a different range than the one previewed while still landing on
/// the previewed object. Verification re-reads the result, but the window is
/// not closed; it is the update counterpart of `apply_install`'s pathname
/// window and is tracked as `skilled-8tr`.
pub(crate) fn apply_repository_update_attempt(plan: &RepositoryUpdatePlan) -> (Result<()>, bool) {
    match validate_repository_update(plan) {
        Ok(()) => (git::fast_forward(&plan.path, &plan.target_revision), true),
        Err(error) => (Err(error), false),
    }
}

fn validate_repository_update(plan: &RepositoryUpdatePlan) -> Result<()> {
    if plan.is_blocked() {
        return Err(crate::Error::SourceChangedAfterPreview);
    }
    let metadata = std::fs::symlink_metadata(&plan.path)
        .map_err(|_| crate::Error::SourceChangedAfterPreview)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || plan.path.canonicalize().ok().as_deref() != Some(plan.path.as_path())
    {
        return Err(crate::Error::SourceChangedAfterPreview);
    }
    if repository_identity(&plan.path).ok().as_ref() != Some(&plan.repository_identity) {
        return Err(crate::Error::SourceChangedAfterPreview);
    }
    let head = git::head_state(&plan.path)?;
    if head.reference() != Some(plan.current_reference.as_str())
        || head.revision() != plan.current_revision
    {
        return Err(crate::Error::SourceChangedAfterPreview);
    }
    let upstream = git::upstream_of(&plan.path, &head)?;
    if upstream.as_ref().map(Upstream::tracking_ref) != Some(plan.upstream_ref.as_str())
        || upstream.as_ref().map(Upstream::revision) != Some(plan.target_revision.as_str())
        || !git::commit_exists(&plan.path, &plan.target_revision)?
    {
        return Err(crate::Error::SourceChangedAfterPreview);
    }
    let state = git::worktree_state(&plan.path)?;
    if state.tracked_dirty() || !state.worktree_dirty_known {
        return Err(crate::Error::SourceChangedAfterPreview);
    }
    if incoming_untracked_collision(&state, &plan.changed_files).is_some() {
        return Err(crate::Error::SourceChangedAfterPreview);
    }
    Ok(())
}

pub fn verify_repository_update(
    plan: &RepositoryUpdatePlan,
    before: &InventorySnapshot,
    after: &InventorySnapshot,
) -> RepositoryVerifyReport {
    let mut failures = Vec::new();
    let mut withheld = Vec::new();
    match git::head_state(&plan.path) {
        Ok(head)
            if head.reference() == Some(plan.current_reference.as_str())
                && head.revision() == plan.target_revision => {}
        Ok(head) => failures.push(format!(
            "HEAD is {} at {}, expected {} at {}",
            head.revision(),
            head.reference().unwrap_or("detached HEAD"),
            plan.target_revision,
            plan.current_reference
        )),
        Err(error) => withheld.push(format!("HEAD could not be checked: {error}")),
    }
    match git::worktree_state(&plan.path) {
        Ok(state) if state.tracked_dirty() => failures.push(
            "the repository has tracked changes after the fast-forward operation".into(),
        ),
        Ok(state) if !state.worktree_dirty_known => withheld.push(
            "post-update worktree cleanliness could not be determined without running configured filters"
                .into(),
        ),
        Ok(_) => {}
        Err(error) => withheld.push(format!("worktree cleanliness could not be checked: {error}")),
    }

    let inventory_complete = |snapshot: &InventorySnapshot| {
        snapshot.counts_are_complete() && snapshot.registry_is_complete()
    };
    if !inventory_complete(before) || !inventory_complete(after) {
        withheld
            .push("the before-and-after installation inventory was not completely readable".into());
    } else {
        let expected_dangling = plan
            .affected
            .expected_dangling
            .iter()
            .map(|(name, agent)| (name.as_str(), *agent))
            .collect::<std::collections::BTreeSet<_>>();
        for row in before.rows() {
            for observation in row
                .observations()
                .filter(|observation| observation.object().is_installation())
            {
                let after_observation = after
                    .row(row.name())
                    .and_then(|row| row.observation(observation.agent()));
                let belongs_to_updated_source = observation
                    .resolution()
                    .is_some_and(|variant| variant.source_id() == plan.source_id);
                if belongs_to_updated_source
                    && expected_dangling.contains(&(row.name(), observation.agent().index()))
                {
                    if !after_observation.is_some_and(|after| {
                        after
                            .findings()
                            .iter()
                            .any(|finding| finding.code() == "install.dangling_symlink")
                    }) {
                        failures.push(format!(
                            "the disclosed removal of {} was not observed as a dangling installation for {}",
                            row.name(),
                            observation.agent().display_name()
                        ));
                    }
                    continue;
                }
                match after_observation {
                    Some(after)
                        if after.health() <= observation.health()
                            && after.resolution() == observation.resolution() => {}
                    Some(after) => failures.push(format!(
                        "installation {} for {} regressed from {} to {} without disclosure",
                        row.name(),
                        observation.agent().display_name(),
                        observation.health().label(),
                        after.health().label()
                    )),
                    None => failures.push(format!(
                        "installation {} for {} disappeared without disclosure",
                        row.name(),
                        observation.agent().display_name()
                    )),
                }
            }
        }
        // An installation that only exists afterwards has no finding to gain
        // and no earlier state to regress from, so neither pass above can see
        // it. A fast-forward writes inside the repository and nowhere near an
        // agent root, so anything that appeared in one — a hook's work, in
        // practice — is by definition undisclosed.
        let installed_before = before
            .rows()
            .iter()
            .flat_map(|row| {
                row.observations()
                    .filter(|observation| observation.object().is_installation())
                    .map(move |observation| (row.name(), observation.agent().index()))
            })
            .collect::<std::collections::BTreeSet<_>>();
        for row in after.rows() {
            for observation in row
                .observations()
                .filter(|observation| observation.object().is_installation())
            {
                if !installed_before.contains(&(row.name(), observation.agent().index())) {
                    failures.push(format!(
                        "installation {} for {} appeared without disclosure",
                        row.name(),
                        observation.agent().display_name()
                    ));
                }
            }
        }
        let finding_key = |entry: crate::inventory::DoctorEntry<'_>| {
            (
                entry.skill_name().to_owned(),
                entry.agent().index(),
                entry.finding().code(),
                entry.finding().evidence().to_owned(),
            )
        };
        let before_findings = before
            .doctor_findings()
            .map(finding_key)
            .collect::<std::collections::BTreeSet<_>>();
        for entry in after.doctor_findings() {
            let key = finding_key(entry);
            let expected_removal = expected_dangling
                .contains(&(entry.skill_name(), entry.agent().index()))
                && entry.finding().code() == "install.dangling_symlink";
            if !expected_removal && !before_findings.contains(&key) {
                failures.push(format!(
                    "installation {} for {} gained undisclosed finding {}: {}",
                    entry.skill_name(),
                    entry.agent().display_name(),
                    entry.finding().code(),
                    entry.finding().evidence()
                ));
            }
        }
    }

    RepositoryVerifyReport {
        verified: failures.is_empty(),
        complete: withheld.is_empty(),
        failures,
        withheld,
    }
}
