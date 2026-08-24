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
        CatalogProposal, RegisteredSource, RepositoryIdentity, SkillValidation,
        repository_identity, repository_identity_from_git_dir,
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
    use super::{gitlink_intersects_catalog, surviving_removal};
    use crate::git;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn run_git(repository: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .output()
            .expect("run Git fixture command");
        assert!(output.status.success(), "git {arguments:?}: {output:?}");
        String::from_utf8(output.stdout)
            .expect("UTF-8 Git output")
            .trim()
            .to_owned()
    }

    /// The skilled-lr8 window, staged deterministically: the checkout is
    /// pinned, then renamed aside and a readable decoy left at its pathname
    /// whose vacating candidate holds only paths the update deletes. The tree
    /// queries are descriptor-bound Git children and answer for the proven
    /// checkout either way; the worktree occupant walk must observe the same
    /// directory, so the occupant standing in the proven checkout is reported
    /// rather than the decoy's vacancy.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn the_occupant_walk_observes_the_pinned_checkout_not_the_pathname() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let checkout = directory.path().join("checkout");
        std::fs::create_dir_all(checkout.join("skills/demo")).expect("create candidate");
        std::fs::write(
            checkout.join("skills/demo/SKILL.md"),
            "---\nname: demo\ndescription: fixture\n---\n# demo\n",
        )
        .expect("write skill document");
        run_git(&checkout, &["init", "-b", "main"]);
        run_git(&checkout, &["config", "user.name", "Skilled Test"]);
        run_git(&checkout, &["config", "user.email", "skilled@example.test"]);
        run_git(&checkout, &["add", "."]);
        run_git(&checkout, &["commit", "-m", "fixture"]);
        // The target revision no longer holds the candidate at all: an empty
        // tree, made without touching the worktree.
        let empty_tree = run_git(&checkout, &["mktree"]);
        let target = run_git(&checkout, &["commit-tree", &empty_tree, "-m", "removal"]);
        // A local occupant the update does not delete keeps the candidate
        // standing in the real checkout.
        std::fs::write(checkout.join("skills/demo/occupant.txt"), "kept\n")
            .expect("write occupant");
        let deleted = std::collections::BTreeSet::from([PathBuf::from("skills/demo/SKILL.md")]);

        let handle = git::RepositoryHandle::open(&checkout).expect("pin the checkout");
        let moved = directory.path().join("moved");
        std::fs::rename(&checkout, &moved).expect("rename the checkout aside");
        // The decoy's candidate holds only the path the update deletes, so a
        // pathname-bound walk would report it vacant.
        std::fs::create_dir_all(checkout.join("skills/demo")).expect("create decoy candidate");
        std::fs::write(
            checkout.join("skills/demo/SKILL.md"),
            "---\nname: demo\ndescription: decoy\n---\n# demo\n",
        )
        .expect("write decoy skill document");

        let occupant = surviving_removal(
            (&handle).into(),
            &target,
            Path::new("skills/demo"),
            &deleted,
        )
        .expect("walk the pinned checkout");

        assert_eq!(occupant, Some(PathBuf::from("skills/demo/occupant.txt")));
    }

    /// The bound descent opens every component with `O_NOFOLLOW`, so a
    /// worktree ancestor that is a symbolic link refuses rather than being
    /// followed to wherever it points: a redirected candidate cannot prove
    /// itself vacant.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn a_symlinked_worktree_ancestor_is_an_occupant_not_a_road() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let checkout = directory.path().join("checkout");
        std::fs::create_dir_all(checkout.join("skills/demo")).expect("create candidate");
        std::fs::write(
            checkout.join("skills/demo/SKILL.md"),
            "---\nname: demo\ndescription: fixture\n---\n# demo\n",
        )
        .expect("write skill document");
        run_git(&checkout, &["init", "-b", "main"]);
        run_git(&checkout, &["config", "user.name", "Skilled Test"]);
        run_git(&checkout, &["config", "user.email", "skilled@example.test"]);
        run_git(&checkout, &["add", "."]);
        run_git(&checkout, &["commit", "-m", "fixture"]);
        let empty_tree = run_git(&checkout, &["mktree"]);
        let target = run_git(&checkout, &["commit-tree", &empty_tree, "-m", "removal"]);
        // The worktree's own `skills` becomes a link to a vacant directory
        // elsewhere; the path still reads, but not inside the checkout.
        let elsewhere = directory.path().join("elsewhere");
        std::fs::create_dir_all(elsewhere.join("demo")).expect("create redirected candidate");
        std::fs::write(
            elsewhere.join("demo/SKILL.md"),
            "---\nname: demo\ndescription: elsewhere\n---\n# demo\n",
        )
        .expect("write redirected skill document");
        std::fs::remove_dir_all(checkout.join("skills")).expect("remove real ancestor");
        std::os::unix::fs::symlink(&elsewhere, checkout.join("skills"))
            .expect("redirect the ancestor");
        let deleted = std::collections::BTreeSet::from([PathBuf::from("skills/demo/SKILL.md")]);
        let handle = git::RepositoryHandle::open(&checkout).expect("pin the checkout");

        let occupant = surviving_removal(
            (&handle).into(),
            &target,
            Path::new("skills/demo"),
            &deleted,
        )
        .expect("walk the pinned checkout");

        assert_eq!(occupant, Some(PathBuf::from("skills/demo")));
    }

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
        // A verification finding reports on the checkout as it stood after a
        // write, and the states it reports on are exactly the ones that would
        // otherwise supersede it: a disclosed post-merge hook that moves the
        // checkout aside produces a source error, one that leaves the checkout
        // unreadable leaves the recorded revision disagreeing with what the
        // next launch reads, and a fast-forward that moved HEAD is meant to
        // have. Letting any of those replace the record would take a failed or
        // unfinished update out of Doctor the moment the report is dismissed,
        // leaving nothing anywhere that says a write was not verified. It says
        // something no observation of the current state can answer, so only a
        // later check may replace it.
        if findings.iter().any(|finding| {
            matches!(
                finding.code(),
                "update.apply_failed"
                    | "update.verification_failed"
                    | "update.verification_incomplete"
            )
        }) {
            return false;
        }
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
                "source.missing"
                    | "source.partial_clone_unsupported"
                    | "source.repository_transport_unsupported"
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
                    | "source.repository_transport_unsupported"
                    | "source.submodule_update_unsupported"
                    | "source.revival_name_mismatch"
                    | "source.changed_after_preview"
                    | "source.no_upstream"
                    | "source.missing"
                    | "source.detached_head"
                    | "update.apply_failed"
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
    inventory: &InventorySnapshot,
    checked_at: i64,
    cancelled: &AtomicBool,
) -> Option<CachedUpdateCheck> {
    let (mut verdict, mut findings) = classify_repository_update(probe);
    let planned = planned_revisions(source, probe, verdict);
    if let Some(finding) = incoming_collision_finding(probe) {
        findings.push(finding);
        verdict = RepositoryUpdateVerdict::Blocked;
    }
    if let Some(finding) = changed_submodule_catalog_finding(source, probe) {
        findings.push(finding);
        verdict = RepositoryUpdateVerdict::Blocked;
    }
    // Every finding the preview would block on is decided here as well, because
    // the cached check is what Updates advertises and what Doctor reads. A
    // finding the preview raises alone would leave the list saying an update is
    // available that the preview then refuses, and Doctor never hearing of it.
    //
    // Reading the target's trees and the candidate's worktree can fail, and an
    // error is left to the plan rather than turned into a check finding of its
    // own: nothing is written on the strength of a check, and the plan asks the
    // same questions again before anything can be.
    let (local, target, changed_files) = planned;
    match affected_catalog_skills(
        source,
        inventory,
        &changed_files,
        (&probe.path).into(),
        &local,
        &target,
        cancelled,
    ) {
        // A cancelled analysis has no answer, and a check recorded without one
        // would be a verdict this source was never actually judged on.
        Ok(None) => return None,
        Ok(Some((_, removal_findings))) if !removal_findings.is_empty() => {
            findings.extend(removal_findings);
            verdict = RepositoryUpdateVerdict::Blocked;
        }
        Ok(Some(_)) | Err(_) => {}
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
    Some(CachedUpdateCheck {
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
            .and_then(|upstream| upstream.revision().map(str::to_owned)),
        merge_base: probe.merge_base.clone(),
        ahead: probe.ahead,
        behind: probe.behind,
        dirty: !dirtiness_withheld && observed_dirty.or_else(|| source.dirty()).unwrap_or(false),
        dirty_known: !dirtiness_withheld && (observed_dirty.is_some() || source.dirty().is_some()),
        verdict,
        detail,
    })
}

const FINDINGS_DETAIL_PREFIX: &str = "findings-v1;";

pub(crate) fn encode_findings(findings: &[Finding]) -> String {
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
        if matches!(code, "update.apply_failed" | "update.verification_failed") {
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
        "source.upstream_unfetched" => "source.upstream_unfetched",
        "source.fetch_failed" => "source.fetch_failed",
        "source.partial_clone_unsupported" => "source.partial_clone_unsupported",
        "source.repository_transport_unsupported" => "source.repository_transport_unsupported",
        "source.submodule_update_unsupported" => "source.submodule_update_unsupported",
        "source.removal_leaves_content" => "source.removal_leaves_content",
        "source.revival_name_mismatch" => "source.revival_name_mismatch",
        "source.changed_after_preview" => "source.changed_after_preview",
        "update.apply_failed" => "update.apply_failed",
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
    // Pinned before anything is proven: the identity check below and every
    // Git process the probe runs go through this handle, so their answers
    // describe one directory rather than whatever the pathname names at each
    // spawn. `skilled-2k3.8.5.1` is the record of why the pathname was not
    // enough.
    let Ok(handle) = git::RepositoryHandle::open(&path) else {
        return changed_checkout_probe(path);
    };
    if !registered_checkout_path_is_current(source, &handle) {
        return changed_checkout_probe(path);
    }
    match probe_existing(&handle, fetch) {
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
    // Pinned before anything is proven, for the reason given on the blocking
    // path: every process this check runs answers for one directory.
    let Ok(handle) = git::RepositoryHandle::open(&path) else {
        return Some(changed_checkout_probe(path));
    };
    let checkout_is_current =
        registered_checkout_path_is_current_cancellable(source, &handle, cancelled, child_slot)?;
    if !checkout_is_current {
        return Some(changed_checkout_probe(path));
    }
    match probe_existing_cancellable(&handle, cancelled, child_slot) {
        Ok(Some(probe)) => Some(probe),
        Ok(None) => None,
        Err(error) => Some(failed_probe_without_reinspection(path, error)),
    }
}

fn probe_existing_cancellable(
    handle: &git::RepositoryHandle,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> std::result::Result<Option<RepositoryUpdateProbe>, ProbeFailure> {
    let path = handle.path();
    let target: git::GitTarget = handle.into();
    let Some(partial_clone) =
        git::repository_is_partial_clone_cancellable(target, cancelled, child_slot)
            .map_err(ProbeFailure::Inspect)?
    else {
        return Ok(None);
    };
    if partial_clone {
        return Ok(Some(partial_clone_probe(path)));
    }
    let Some(transport_code) =
        git::repository_transport_code_cancellable(target, cancelled, child_slot)
            .map_err(ProbeFailure::Inspect)?
    else {
        return Ok(None);
    };
    if let Some(setting) = transport_code {
        return Ok(Some(transport_code_probe(path, &setting)));
    }
    let Some(local) = git::head_state_cancellable(target, cancelled, child_slot)
        .map_err(ProbeFailure::Inspect)?
    else {
        return Ok(None);
    };
    let Some(worktree) = git::worktree_state_cancellable(target, cancelled, child_slot)
        .map_err(ProbeFailure::Inspect)?
    else {
        return Ok(None);
    };
    let Some(upstream) = git::upstream_of_cancellable(target, &local, cancelled, child_slot)
        .map_err(ProbeFailure::Inspect)?
    else {
        return Ok(None);
    };
    let Some(upstream) = upstream else {
        return Ok(Some(no_upstream_probe(path, local, worktree)));
    };
    let revision = if upstream.remote() == "." {
        // A local upstream has no fetch to populate its ref, so `upstream_of`
        // already declined to report one whose revision is absent.
        let Some(revision) = upstream.revision() else {
            return Ok(Some(unfetched_upstream_probe(
                path, local, worktree, upstream,
            )));
        };
        revision.to_owned()
    } else {
        // The URL Git would actually fetch, after `insteadOf` rewrites and
        // over a remote that may list several.
        let key = format!("remote.{}.url", upstream.remote());
        let Some(url) =
            git::effective_remote_url_cancellable(target, upstream.remote(), cancelled, child_slot)
                .map_err(ProbeFailure::Inspect)?
        else {
            return Ok(None);
        };
        if url.as_deref().is_some_and(git::remote_url_runs_a_helper) {
            return Ok(Some(transport_code_probe(path, &key)));
        }
        // Re-asked immediately before the fetch, for the reason given on the
        // same guard in `probe_existing`.
        let Some(transport_code) =
            git::repository_transport_code_cancellable(target, cancelled, child_slot)
                .map_err(ProbeFailure::Inspect)?
        else {
            return Ok(None);
        };
        if let Some(setting) = transport_code {
            return Ok(Some(transport_code_probe(path, &setting)));
        }
        match git::fetch_upstream_cancellable(target, &upstream, cancelled, child_slot) {
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
    let upstream = upstream.with_revision(revision.clone());
    let Some(base) =
        git::merge_base_cancellable(target, local.revision(), &revision, cancelled, child_slot)
            .map_err(ProbeFailure::Inspect)?
    else {
        return Ok(None);
    };
    let Some(counts) =
        git::ahead_behind_cancellable(target, local.revision(), &revision, cancelled, child_slot)
            .map_err(ProbeFailure::Inspect)?
    else {
        return Ok(None);
    };
    let changed_files = if counts.behind > 0 {
        let Some(files) = git::changed_paths_cancellable(
            target,
            local.revision(),
            &revision,
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
    // Pinned before anything is proven, for the reason given on the explicit
    // check: every process this re-probe runs answers for one directory.
    let Ok(handle) = git::RepositoryHandle::open(&path) else {
        return changed_checkout_probe(path);
    };
    if !registered_checkout_path_is_current(source, &handle) {
        return changed_checkout_probe(path);
    }
    match probe_existing_against(&handle, upstream_ref, upstream_revision) {
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
    let local = git::head_state((&path).into()).ok();
    let worktree = git::worktree_state((&path).into()).ok();
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

/// The refusal a checkout earns by configuring a program for Git to run.
///
/// `setting` is named because the user is the one who has to act on it, and
/// the name is the only part of the configuration that is safe to repeat: a
/// value is a command line the checkout wrote, and printing it would put that
/// text on the user's terminal.
fn transport_code_probe(path: &Path, setting: &str) -> RepositoryUpdateProbe {
    RepositoryUpdateProbe {
        path: path.into(),
        local: None,
        upstream: None,
        merge_base: None,
        ahead: 0,
        behind: 0,
        worktree: None,
        changed_files: Vec::new(),
        error: Some(format!(
            "source.repository_transport_unsupported|the registered checkout configures {setting}, which would run a program it chose during the check"
        )),
    }
}

/// Whether the registered pathname still names the registered repository —
/// asked through `handle`, so the identity answered for is the pinned
/// directory the rest of the probe will act on, not whatever the pathname
/// resolves to at each later spawn.
fn registered_checkout_path_is_current(
    source: &RegisteredSource,
    handle: &git::RepositoryHandle,
) -> bool {
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
    let Ok(git_dir) = git::repository_git_dir(handle.into()) else {
        return false;
    };
    source.repository_identity().is_some_and(|expected| {
        repository_identity_from_git_dir(git_dir).is_ok_and(|current| current == *expected)
    })
}

fn registered_checkout_path_is_current_cancellable(
    source: &RegisteredSource,
    handle: &git::RepositoryHandle,
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
    let git_dir = match git::repository_git_dir_cancellable(handle.into(), cancelled, child_slot) {
        Ok(Some(path)) => path,
        Ok(None) => return None,
        Err(_) => return Some(false),
    };
    Some(source.repository_identity().is_some_and(|expected| {
        repository_identity_from_git_dir(git_dir).is_ok_and(|current| current == *expected)
    }))
}

fn probe_existing_against(
    handle: &git::RepositoryHandle,
    expected_upstream_ref: &str,
    upstream_revision: &str,
) -> Result<RepositoryUpdateProbe> {
    let path = handle.path();
    let target: git::GitTarget = handle.into();
    // Re-asked, not remembered. The explicit check refused a partial clone
    // before reading any object, but a promisor remote configured since then
    // would let `merge-base`, `diff-tree`, and `rev-list` below fetch missing
    // objects lazily — network access while the user is merely opening a
    // preview, outside the one step that was agreed to reach the network.
    if git::repository_is_partial_clone(target)? {
        return Ok(partial_clone_probe(path));
    }
    let local = git::head_state(target)?;
    let worktree = git::worktree_state(target)?;
    let Some(upstream) = git::upstream_of(target, &local)? else {
        return Ok(no_upstream_probe(path, local, worktree));
    };
    if upstream.tracking_ref() != expected_upstream_ref {
        return Err(crate::Error::SourceChangedAfterPreview);
    }
    // An absent tracking ref reads the same way a moved one does: the object
    // the plan was previewed against is not what the ref names now.
    if upstream.revision() != Some(upstream_revision) {
        return Err(crate::Error::SourceChangedAfterPreview);
    }
    finish_probe(
        target,
        local,
        worktree,
        upstream.with_revision(upstream_revision.to_owned()),
    )
}

fn probe_existing(
    handle: &git::RepositoryHandle,
    fetch: bool,
) -> std::result::Result<RepositoryUpdateProbe, ProbeFailure> {
    let path = handle.path();
    let target: git::GitTarget = handle.into();
    if git::repository_is_partial_clone(target).map_err(ProbeFailure::Inspect)? {
        return Ok(partial_clone_probe(path));
    }
    // Asked before anything else the check does, because the answer decides
    // whether the check may run at all rather than how one of its steps
    // behaves.
    if let Some(setting) = git::repository_transport_code(target).map_err(ProbeFailure::Inspect)? {
        return Ok(transport_code_probe(path, &setting));
    }
    let local = git::head_state(target).map_err(ProbeFailure::Inspect)?;
    let worktree = git::worktree_state(target).map_err(ProbeFailure::Inspect)?;
    let mut upstream = git::upstream_of(target, &local).map_err(ProbeFailure::Inspect)?;
    if fetch
        && let Some(value) = &upstream
        && value.remote() != "."
    {
        // The URL is the remaining way a checkout names a program, and it has
        // to be the URL Git would actually fetch rather than the one a plain
        // read reports: `insteadOf` rewrites the value on the way to the
        // transport, and a remote with several URLs is fetched from the first
        // while `--get` answers with the last.
        if let Some(url) =
            git::effective_remote_url(target, value.remote()).map_err(ProbeFailure::Inspect)?
            && git::remote_url_runs_a_helper(&url)
        {
            return Ok(transport_code_probe(
                path,
                &format!("remote.{}.url", value.remote()),
            ));
        }
        // Re-asked immediately before the fetch, not remembered from the top of
        // this function. Two of the keys are now settled at the moment they are
        // used rather than in advance — Git enforces the transport allowlist
        // itself, and `core.sshCommand` is read with its scope — but the rest
        // are read by Git out of whatever the config holds when the fetch
        // starts, so the head, worktree, upstream, and URL reads above must not
        // be inside the window this answer covers. What remains is the single
        // gap between this process and the fetch, tracked as `skilled-88j`.
        if let Some(setting) =
            git::repository_transport_code(target).map_err(ProbeFailure::Inspect)?
        {
            return Ok(transport_code_probe(path, &setting));
        }
        let revision = git::fetch_upstream(target, value).map_err(ProbeFailure::Fetch)?;
        upstream = Some(value.with_revision(revision));
    }
    let Some(upstream_value) = upstream else {
        return Ok(no_upstream_probe(path, local, worktree));
    };
    finish_probe(target, local, worktree, upstream_value).map_err(ProbeFailure::Inspect)
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

/// A configured upstream whose remote-tracking ref this probe did not fetch.
///
/// Distinct from [`no_upstream_probe`], which says the branch tracks nothing:
/// here the remote, the merge ref, and the fetch mapping are all configured and
/// only the ref's object is missing. Saying "no upstream" would send the user
/// to reconfigure a branch that is configured correctly, and would hide that an
/// explicit check is the thing that would resolve it.
fn unfetched_upstream_probe(
    path: &Path,
    local: HeadState,
    worktree: WorktreeState,
    upstream: Upstream,
) -> RepositoryUpdateProbe {
    RepositoryUpdateProbe {
        path: path.into(),
        error: Some(format!(
            "source.upstream_unfetched|{} names no object here yet",
            upstream.tracking_ref()
        )),
        local: Some(local),
        upstream: Some(upstream),
        merge_base: None,
        ahead: 0,
        behind: 0,
        worktree: Some(worktree),
        changed_files: Vec::new(),
    }
}

fn finish_probe(
    target: git::GitTarget<'_>,
    local: HeadState,
    worktree: WorktreeState,
    upstream: Upstream,
) -> Result<RepositoryUpdateProbe> {
    let path = target.path();
    let Some(revision) = upstream.revision().map(str::to_owned) else {
        return Ok(unfetched_upstream_probe(path, local, worktree, upstream));
    };
    let base = git::merge_base(target, local.revision(), &revision)?;
    let counts = git::ahead_behind(target, local.revision(), &revision)?;
    let changed_files = if counts.behind > 0 {
        git::changed_paths(target, local.revision(), &revision)?
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
    /// Installations a dangling link holds that the update gives a target to,
    /// as the name the agent root holds and the upstream directory it will
    /// point into. The two differ whenever a link was installed under another
    /// name.
    ///
    /// That the target will exist is what has been established, and it is all
    /// that is claimed: whether the directory the update creates validates as
    /// a skill is not read here, so a link may gain a target and still be
    /// unloadable. Verification reads the result and reports that on its own
    /// account, the same as for any other installation.
    pub restored: Vec<(String, String)>,
    /// One upstream skill moving to another name, beside the installations the
    /// move leaves without a target that the pair does not already name. Every
    /// surface states that consequence rather than only the name: it is what
    /// verification holds the update to, so it is what the confirmation covers.
    pub renamed: Vec<(String, String, Vec<String>)>,
    pub complete: bool,
    pub incomplete_reason: Option<String>,
    expected_dangling: Vec<(String, usize)>,
    /// The installations the plan said this update gives a target to, as the
    /// row and agent that will start loading and the upstream skill it was
    /// disclosed as loading.
    ///
    /// The variant's own path inside the source is carried because the raw
    /// link target is not enough to identify what the link resolves to. The
    /// planner follows intermediate symbolic links to find a dangling link's
    /// destination, so a hook or a concurrent process can retarget one of
    /// those aliases at a different variant in the same registered source: the
    /// agent link's own target is unchanged and the source still matches, and
    /// without this there is nothing left to disagree with the plan. The path
    /// rather than the name, because one source can hold same-named variants
    /// in different catalog roots or agent editions.
    expected_revival: Vec<(String, usize, String, PathBuf)>,
    /// Every candidate this update was disclosed as emptying, relative to the
    /// checkout, whether by removal or as the old side of a rename.
    ///
    /// The occupant check that cleared them is a preview-time read, so the
    /// guard before the write re-reads them from fresh worktree state: a file
    /// dropped into one between the preview and the confirmation would leave
    /// the directory standing and the confirmed outcome unmet.
    vacating_candidates: Vec<PathBuf>,
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

/// What an update would move between, and what it would change on the way.
///
/// A verdict other than `Available` has no target to state, so the update is
/// described as going nowhere and changing nothing rather than as advancing to
/// an upstream the classifier already refused. Shared with
/// [`cached_update_check`], which has to reach the same answer from the same
/// probe or the check and the preview would disagree about what the update is.
fn planned_revisions(
    source: &RegisteredSource,
    probe: &RepositoryUpdateProbe,
    verdict: RepositoryUpdateVerdict,
) -> (String, String, Vec<ChangedPath>) {
    let local = probe
        .local
        .as_ref()
        .map(HeadState::revision)
        .unwrap_or(source.head())
        .to_owned();
    let available = verdict == RepositoryUpdateVerdict::Available;
    let target = if available {
        probe
            .upstream
            .as_ref()
            .and_then(Upstream::revision)
            .unwrap_or(&local)
    } else {
        &local
    }
    .to_owned();
    let changed_files = if available {
        probe.changed_files.clone()
    } else {
        Vec::new()
    };
    (local, target, changed_files)
}

pub fn plan_repository_update(
    source: &RegisteredSource,
    probe: &RepositoryUpdateProbe,
    inventory: &InventorySnapshot,
) -> Result<RepositoryUpdatePlan> {
    let (verdict, mut findings) = classify_repository_update(probe);
    let (local, target, changed_files) = planned_revisions(source, probe, verdict);
    let upstream_ref = probe
        .upstream
        .as_ref()
        .map(Upstream::tracking_ref)
        .unwrap_or("")
        .to_owned();
    let commits = if verdict == RepositoryUpdateVerdict::Available {
        git::commit_summaries((&probe.path).into(), &local, &target)?
    } else {
        Vec::new()
    };
    if let Some(finding) = incoming_collision_finding(probe) {
        findings.push(finding);
    }
    if let Some(finding) = changed_submodule_catalog_finding(source, probe) {
        findings.push(finding);
    }
    // Uncancellable by construction: planning is the step a confirmation is
    // about to be asked for, and it runs where an effect may block.
    let (affected, removal_findings) = affected_catalog_skills(
        source,
        inventory,
        &changed_files,
        (&probe.path).into(),
        &local,
        &target,
        &AtomicBool::new(false),
    )?
    .expect("a flag that is never set cannot cancel this analysis");
    findings.extend(removal_findings);
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
        // Filters belong beside the hooks, and so does the filesystem monitor.
        // `git::fast_forward` deliberately hands Git the repository's own
        // configuration, so materialising the updated paths can run a
        // configured smudge or process filter — local commands, and in the case
        // of Git LFS further network access — and a `core.fsmonitor` hook runs
        // several times over the same merge. The check suppresses repository
        // code precisely because it was not agreed to; what the fast-forward
        // runs is agreed to, which requires naming all of it.
        //
        // Signature verification belongs in the same sentence and for the same
        // reason. `merge.verifySignatures` runs the configured `gpg.program`
        // over the incoming tip, and the merge is not given a flag either way:
        // the setting is a policy, one Skilled has no standing to overrule
        // where the user set it themselves, so what it may run is named rather
        // than turned off.
        hooks_disclosure: "Git may run your post-merge and reference-transaction hooks, your \
             core.fsmonitor hook, any checkout filters this repository configures, and — where \
             signature verification is configured — the signature program it names, during \
             this fast-forward."
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

/// `Ok(None)` is a cancelled analysis, not an empty one. The tree reads below
/// are one local Git process each and there is one for every candidate the
/// update touches, so a large update spends real time here; the explicit check
/// runs it on the cancellable worker, and a Cancel that had to wait for all of
/// it would not be a cancel. Nothing partial is ever returned — a caller that
/// sees `None` has no answer to record.
fn affected_catalog_skills(
    source: &RegisteredSource,
    inventory: &InventorySnapshot,
    files: &[ChangedPath],
    repository: git::GitTarget<'_>,
    current_revision: &str,
    target_revision: &str,
    cancelled: &AtomicBool,
) -> Result<Option<(AffectedInstallations, Vec<Finding>)>> {
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

    // A directory is not a skill; the `SKILL.md` in it is. Asking whether the
    // directory exists calls a candidate retained after upstream deleted its
    // skill document and left another tracked file beside it, and for a catalog
    // whose skill is the repository root it can only ever answer yes. The
    // preview would then say "updated in place" about an installation the
    // update stops an agent from loading, and only verification, after the
    // write, would find out.
    let candidate_holds_skill = |revision: &str, path: &Path| -> Result<bool> {
        git::tree_regular_file_exists(repository, revision, &candidate_skill_document(path))
    };

    // A rename is stated only when Git proved every path in both changed
    // directories to be an exact-content rename. Partial or edited moves stay
    // the conservative removed-plus-added pair.
    let mut rename_candidates = Vec::new();
    for ((catalog_path, old_name, new_name), pairs) in &rename_evidence {
        if is_cancelled(cancelled) {
            return Ok(None);
        }
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
            && !candidate_holds_skill(target_revision, &old_change.candidate_path)?
            && !candidate_holds_skill(current_revision, &new_change.candidate_path)?
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

    // Keyed by the variant an installation resolves to, because that is what an
    // upstream change is about; the name the root holds is carried in the value
    // instead. A link installed as `alias` pointing at skill `demo` is keyed
    // under `demo` here and disclosed as `alias`, where keying by the root
    // entry's own name would have matched neither and left the installation out
    // of the preview altogether.
    let mut installed = std::collections::BTreeMap::<
        (PathBuf, String),
        std::collections::BTreeSet<(String, usize)>,
    >::new();
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
                    variant.skill_name().to_owned(),
                ))
                .or_default()
                .insert((row.name().to_owned(), observation.agent().index()));
        }
    }
    let mut updated = Vec::new();
    let mut removed_changes = Vec::new();
    let mut added_changes = Vec::new();
    let mut restored = Vec::new();
    let mut expected_revival = Vec::new();
    let mut revival_name_mismatches = Vec::new();
    for ((catalog_path, name), change) in &changes {
        if is_cancelled(cancelled) {
            return Ok(None);
        }
        let key = (catalog_path.clone(), name.clone());
        if confirmed_renames
            .iter()
            .any(|(old, new)| old == &key || new == &key)
        {
            continue;
        }
        let target_keeps_skill = candidate_holds_skill(target_revision, &change.candidate_path)?;
        // A file added inside a skill that was already there does not make the
        // skill new. Only a candidate the current revision does not have can be
        // stated as added upstream; a catalog whose skill is its root always
        // has one, so nothing about it is ever reported that way.
        let skill_is_new = !candidate_holds_skill(current_revision, &change.candidate_path)?;
        // A dangling link resolves to nothing, so it carries no variant and
        // none of the passes above can see it. It is still an installation,
        // and one aimed at exactly the directory this update creates: after
        // the fast-forward it loads, which is a change to what an agent reads
        // and has to be disclosed. Left out, the preview would call the skill
        // uninstalled and verification would then read the link resolving as
        // an undisclosed change, failing an update that did precisely what it
        // said.
        //
        // Asked before anything about what is installed under the name, and
        // independently of it. `installed` is keyed by the name a root holds,
        // and one root's link under that name may already resolve to another
        // variant entirely while another root's link of the same name dangles
        // at the directory being created. What decides a revival is the
        // directory appearing, not what some other agent's link happens to
        // answer to.
        let (revived, mismatched) = if target_keeps_skill && skill_is_new {
            revived_by_added_skill(
                inventory,
                name,
                &absolute_candidate_path(repository.path(), &change.candidate_path),
            )
        } else {
            (Vec::new(), Vec::new())
        };
        revival_name_mismatches.extend(
            mismatched
                .into_iter()
                .map(|installed| (installed, name.clone())),
        );
        if !revived.is_empty() {
            // The installation that starts loading is the one the root holds,
            // which need not be named after the directory it points at. Naming
            // the upstream skill alone would have the user confirm a plan that
            // does not mention the installation it changes, and would collapse
            // several links aimed at one directory into a single line.
            restored.extend(revived.iter().map(|(row, _)| (row.clone(), name.clone())));
            expected_revival.extend(revived.iter().map(|(row, agent)| {
                (
                    row.clone(),
                    *agent,
                    name.clone(),
                    change.candidate_path.clone(),
                )
            }));
        }
        let installed_agents = installed.get(&(catalog_path.clone(), name.clone()));
        if installed_agents.is_none() {
            if target_keeps_skill && skill_is_new && revived.is_empty() {
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
            updated.extend(
                installed_agents
                    .into_iter()
                    .flatten()
                    .map(|(row, _)| row.clone()),
            );
        }
    }

    let mut expected_dangling = Vec::new();
    let mut surviving_removals = Vec::new();
    let mut vacating_candidates = Vec::new();
    let mut removed = Vec::new();
    let deleted = deleted_paths(files);
    for (name, change, entries) in removed_changes {
        if is_cancelled(cancelled) {
            return Ok(None);
        }
        vacating_candidates.push(change.candidate_path.clone());
        if let Some(occupant) = surviving_removal(
            repository,
            target_revision,
            &change.candidate_path,
            &deleted,
        )? {
            surviving_removals.push((name.clone(), occupant));
        }
        expected_dangling.extend(entries.iter().map(|(row, agent)| (row.clone(), *agent)));
        removed.extend(entries.into_iter().map(|(row, _)| row));
    }
    let mut added = added_changes
        .into_iter()
        .map(|(name, _)| name)
        .collect::<Vec<_>>();
    updated.sort();
    updated.dedup();
    removed.sort();
    removed.dedup();
    added.sort();
    added.dedup();
    let mut renamed = Vec::new();
    for ((catalog_path, old), (_, new)) in confirmed_renames {
        if is_cancelled(cancelled) {
            return Ok(None);
        }
        // The destination of a rename is a directory the update creates, so a
        // dangling link already aimed there is revived by it exactly as one
        // aimed at a plain addition is. Both sides of a rename are skipped by
        // the loop above, so this is the only place that can see it — and the
        // two are independent: the same update can leave the old skill's link
        // dangling and make another link at the new path start loading.
        if let Some(new_change) = changes.get(&(catalog_path.clone(), new.clone())) {
            let (revived, mismatched) = revived_by_added_skill(
                inventory,
                &new,
                &absolute_candidate_path(repository.path(), &new_change.candidate_path),
            );
            revival_name_mismatches.extend(
                mismatched
                    .into_iter()
                    .map(|installed| (installed, new.clone())),
            );
            restored.extend(revived.iter().map(|(row, _)| (row.clone(), new.clone())));
            expected_revival.extend(
                revived.into_iter().map(|(row, agent)| {
                    (row, agent, new.clone(), new_change.candidate_path.clone())
                }),
            );
        }
        // A rename nothing is installed from changes nothing about what any
        // agent loads, so it is not an affected installation and saying so
        // would claim an effect on the user's agents that will not happen. A
        // link revived at the destination is a separate fact, and `restored`
        // above is where it is stated.
        let Some(entries) = installed.get(&(catalog_path.clone(), old.clone())) else {
            continue;
        };
        // The old side of a rename empties its candidate exactly as a removal
        // does, and is disclosed as the same outcome — an installation left
        // without a target — so a local occupant that keeps the old directory
        // standing breaks the same promise, and is refused the same way.
        if let Some(old_change) = changes.get(&(catalog_path, old.clone())) {
            vacating_candidates.push(old_change.candidate_path.clone());
            if let Some(occupant) = surviving_removal(
                repository,
                target_revision,
                &old_change.candidate_path,
                &deleted,
            )? {
                surviving_removals.push((old.clone(), occupant));
            }
        }
        // Stated as the repository fact it is — one upstream skill moving to
        // another name — beside the installations it leaves without a target.
        // Those are carried by the name each root holds, which is what
        // verification looks one up by, and a link installed under a name of
        // its own would otherwise be held to an outcome the confirmation never
        // mentioned. A name equal to the old skill's is already stated by the
        // pair, so only the ones the pair does not name are listed.
        expected_dangling.extend(entries.iter().map(|(row, agent)| (row.clone(), *agent)));
        let mut aliases = entries
            .iter()
            .map(|(row, _)| row.clone())
            .filter(|row| *row != old)
            .collect::<Vec<_>>();
        aliases.sort();
        aliases.dedup();
        renamed.push((old, new, aliases));
    }
    renamed.sort();
    renamed.dedup();
    expected_dangling.sort();
    expected_dangling.dedup();
    restored.sort();
    restored.dedup();
    expected_revival.sort();
    expected_revival.dedup();
    let findings = surviving_removals
        .into_iter()
        .map(|(name, occupant)| {
            Finding::new(
                "source.removal_leaves_content",
                FindingSeverity::Warning,
                format!(
                    "the update stops {} being a skill, and {} would still be standing \
                     afterwards, so the installation would resolve to something that is not \
                     a skill rather than losing its target",
                    name,
                    occupant.display()
                ),
            )
        })
        .chain(revival_name_mismatches.into_iter().map(|(installed, skill)| {
            Finding::new(
                "source.revival_name_mismatch",
                FindingSeverity::Warning,
                format!(
                    "installation {installed} points at skill {skill}, but its different name \
                     means the update would change its invalid state without making it load"
                ),
            )
        }))
        .collect();
    Ok(Some((
        AffectedInstallations {
            updated,
            removed,
            added,
            restored,
            renamed,
            complete: inventory.counts_are_complete() && inventory.registry_is_complete(),
            incomplete_reason: if inventory.counts_are_complete()
                && inventory.registry_is_complete()
            {
                None
            } else if inventory.scan_pending() {
                Some(
                    "installation inventory has not been scanned; setup may not be complete".into(),
                )
            } else if inventory.no_agent_configured() {
                Some("no agent is configured for installation scanning".into())
            } else {
                Some("one or more installation roots could not be fully read".into())
            },
            expected_dangling,
            expected_revival,
            vacating_candidates,
        },
        findings,
    )))
}

fn is_cancelled(cancelled: &AtomicBool) -> bool {
    cancelled.load(std::sync::atomic::Ordering::Acquire)
}

/// The skill document a catalog candidate is a skill by virtue of holding.
///
/// A catalog whose skill is its root has `.` for a candidate path, and the
/// document it is a skill by is the repository's own `SKILL.md`.
fn candidate_skill_document(candidate: &Path) -> PathBuf {
    if candidate == Path::new(".") {
        PathBuf::from("SKILL.md")
    } else {
        candidate.join("SKILL.md")
    }
}

/// What would still be standing where a disclosed removal takes a skill away.
///
/// `merge --ff-only` deletes the tracked files under a removed directory and
/// removes the directory only once nothing is left in it. Three things leave it
/// standing: a catalog whose skill is the repository root, which no update can
/// remove; another tracked path the target revision still keeps under the
/// candidate; and a local untracked or ignored file sitting in it, which Git
/// leaves exactly where it is. In each case the installed link keeps resolving
/// — to a directory that is no longer a skill — so the dangling link the
/// preview would promise is not what the write produces. That is a plan that
/// cannot state its own outcome, and it blocks rather than being applied and
/// discovered afterwards.
///
/// A directory the walk could not read cannot rule an occupant out, so it is
/// treated as one. So is one too large to walk within [`OCCUPANT_BUDGET`]:
/// understating what is standing there is the error that gets a write applied.
fn surviving_removal(
    repository: git::GitTarget<'_>,
    target_revision: &str,
    candidate: &Path,
    deleted: &std::collections::BTreeSet<PathBuf>,
) -> Result<Option<PathBuf>> {
    if candidate == Path::new(".") {
        return Ok(Some(repository.path().to_path_buf()));
    }
    // Any entry, not only a directory: an update that replaces the skill
    // directory with a regular file or a symbolic link leaves the installed
    // link resolving to that object rather than losing its target, which is the
    // same promise broken in the same way.
    if git::tree_entry_exists(repository, target_revision, candidate)? {
        return Ok(Some(candidate.to_path_buf()));
    }
    // And an ancestor the update turns into a symbolic link redirects the path
    // without ever appearing at it: `ls-tree` does not walk through a link, so
    // the candidate reads as absent while the installed link would follow the
    // new ancestor to wherever upstream aimed it — outside the registered
    // checkout, for all this can tell. An ancestor the update deletes outright
    // is nothing to redirect through, so absence keeps looking upward rather
    // than settling the question.
    for ancestor in candidate.ancestors().skip(1) {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        if git::tree_directory_entry(repository, target_revision, ancestor)? == Some(false) {
            return Ok(Some(ancestor.to_path_buf()));
        }
    }
    // The worktree walk observes the same directory the tree queries above
    // answered for. Handed the pinned handle, it descends with
    // `openat(2)`-relative descriptors from the held directory — the
    // skilled-lr8 window, where a checkout renamed aside and replaced around
    // this walk could clear a vacating-candidate guard with a readable,
    // vacant decoy, is closed on Linux and macOS — the Unix platforms
    // Skilled supports — by never consulting the pathname; the listing
    // leans on `d_type` and an errno accessor, so it is compiled for
    // exactly the platforms whose C library layout it implements. Handed
    // a pathname — every preview-time call, which promises nothing — or
    // on any other platform, it re-resolves the pathname exactly as
    // before.
    let mut budget = OCCUPANT_BUDGET;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let git::GitTarget::Handle(handle) = repository {
        return match handle.open_worktree_directory(candidate) {
            Ok(directory) => {
                surviving_worktree_entry_bound(&directory, candidate, deleted, &mut budget)
            }
            // A candidate that cannot be opened from the held directory —
            // absent, unreadable, or redirected through a symbolic link on
            // the way — cannot rule an occupant out, so it is treated as one,
            // the same answer the pathname walk gives an unreadable
            // directory.
            Err(_) => Ok(Some(candidate.to_path_buf())),
        };
    }
    surviving_worktree_entry(
        &repository.path().join(candidate),
        candidate,
        deleted,
        &mut budget,
    )
}

/// How many worktree entries [`surviving_removal`] reads before giving up and
/// reporting that it could not establish the candidate would go away.
const OCCUPANT_BUDGET: usize = 4096;

/// The first live path under `directory` that this update does not delete, and
/// which therefore keeps the directory standing.
///
/// Git's untracked and ignored lists cannot answer this on their own: they name
/// files, and a directory with no files anywhere beneath it appears in neither
/// while Git leaves it exactly where it is. A directory that holds only paths
/// the update deletes is removed once they are gone; a directory that is
/// already empty is not, because the update deletes nothing in it. Both fall
/// out of asking, at every level, whether anything here is not on the way out.
///
/// Symbolic links are read as entries, never followed: what a link points at is
/// not under this directory, and following one could leave the walk going in a
/// circle.
fn surviving_worktree_entry(
    directory: &Path,
    relative: &Path,
    deleted: &std::collections::BTreeSet<PathBuf>,
    budget: &mut usize,
) -> Result<Option<PathBuf>> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Ok(Some(relative.to_path_buf()));
    };
    let mut empty = true;
    for entry in entries {
        empty = false;
        if *budget == 0 {
            return Ok(Some(relative.to_path_buf()));
        }
        *budget -= 1;
        let Ok(entry) = entry else {
            return Ok(Some(relative.to_path_buf()));
        };
        let path = relative.join(entry.file_name());
        let Ok(file_type) = entry.file_type() else {
            return Ok(Some(path));
        };
        if file_type.is_dir() {
            if let Some(occupant) = surviving_worktree_entry(&entry.path(), &path, deleted, budget)?
            {
                return Ok(Some(occupant));
            }
        } else if !deleted.contains(&path) {
            return Ok(Some(path));
        }
    }
    Ok(empty.then(|| relative.to_path_buf()))
}

/// [`surviving_worktree_entry`], bound to the pinned checkout: the same
/// question, asked through descriptors instead of pathnames. The directory
/// handed in was opened from the held checkout descriptor, recursion opens
/// each subdirectory relative to its parent's descriptor, and the listing
/// itself reads through the descriptor too, so no rename of the checkout can
/// change which directory any level of the walk observes. The answers keep
/// the pathname walk's shape exactly: an empty directory is an occupant, a
/// listing or entry that cannot be established is an occupant, and a budget
/// that runs out reports the directory rather than understating it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn surviving_worktree_entry_bound(
    directory: &std::fs::File,
    relative: &Path,
    deleted: &std::collections::BTreeSet<PathBuf>,
    budget: &mut usize,
) -> Result<Option<PathBuf>> {
    let Ok(listed) = git::bound_directory_entries(directory, *budget) else {
        return Ok(Some(relative.to_path_buf()));
    };
    // More entries than the remaining budget: reported as the budget running
    // out, exactly where the pathname walk would have stopped reading.
    let Some(entries) = listed else {
        return Ok(Some(relative.to_path_buf()));
    };
    if entries.is_empty() {
        return Ok(Some(relative.to_path_buf()));
    }
    for entry in entries {
        if *budget == 0 {
            return Ok(Some(relative.to_path_buf()));
        }
        *budget -= 1;
        let path = relative.join(&entry.name);
        if entry.is_directory {
            let Ok(child) = git::open_directory_at(directory, &entry.name) else {
                return Ok(Some(path));
            };
            if let Some(occupant) = surviving_worktree_entry_bound(&child, &path, deleted, budget)?
            {
                return Ok(Some(occupant));
            }
        } else if !deleted.contains(&path) {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

/// The paths an update deletes from the worktree, rename sources included.
fn deleted_paths(files: &[ChangedPath]) -> std::collections::BTreeSet<PathBuf> {
    files
        .iter()
        .flat_map(|file| {
            file.renamed_from()
                .map(Path::to_path_buf)
                .into_iter()
                .chain(
                    matches!(file.kind(), ChangeKind::Deleted).then(|| file.path().to_path_buf()),
                )
        })
        .collect()
}

/// Where a catalog candidate sits in the checkout, as an absolute path.
///
/// A catalog whose skill is its root has `.` for a candidate path, and the
/// directory it names is the repository itself.
fn absolute_candidate_path(repository: &Path, candidate: &Path) -> PathBuf {
    if candidate == Path::new(".") {
        repository.to_path_buf()
    } else {
        repository.join(candidate)
    }
}

/// The dangling links aimed at `directory`, which would therefore start
/// resolving once the update creates it, as the row name and agent each was
/// observed under.
///
/// The row name is carried rather than the upstream skill's, because a link
/// may be installed under a different name than the directory it points at and
/// verification looks the installation up by the name the root holds.
///
/// The link's recorded target is compared as it was read, exactly as repair
/// compares one against a receipt. Nothing here consults a receipt or claims
/// ownership: this is a statement about what an agent will load after the
/// fast-forward, which the scan can see from the target alone.
fn revived_by_added_skill(
    inventory: &InventorySnapshot,
    skill_name: &str,
    directory: &Path,
) -> (Vec<(String, usize)>, Vec<String>) {
    let mut revived = Vec::new();
    let mut mismatched = Vec::new();
    for row in inventory.rows() {
        for observation in row.observations() {
            let crate::inventory::InstallationObject::Symlink { target } = observation.object()
            else {
                continue;
            };
            if !matches!(
                observation.validation(),
                Some(SkillValidation::Valid { .. })
            ) && link_target_names_directory(observation.path(), target, directory)
            {
                if row.name() == skill_name {
                    revived.push((row.name().to_owned(), observation.agent().index()));
                } else {
                    mismatched.push(row.name().to_owned());
                }
            }
        }
    }
    revived.sort();
    revived.dedup();
    mismatched.sort();
    mismatched.dedup();
    (revived, mismatched)
}

fn link_target_names_directory(link: &Path, target: &Path, directory: &Path) -> bool {
    let resolved_target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        let Some(parent) = link.parent() else {
            return false;
        };
        parent.join(target)
    };
    match (resolved_target.canonicalize(), directory.canonicalize()) {
        (Ok(target), Ok(directory)) => target == directory,
        _ => matches!(
            (
                absent_directory_key(link.parent(), target),
                absent_directory_key(None, directory)
            ),
            (Some(target), Some(directory)) if target == directory
        ),
    }
}

/// A comparable identity for a directory that does not exist yet.
///
/// `canonicalize` needs the path to be there, and the whole point of both
/// sides here is that it is not: the update has yet to create the skill, and
/// the link pointing at it dangles. The parent does exist, so the pair of the
/// resolved parent and the final component names the same directory whichever
/// spelling each side was recorded with — a temporary directory reached as
/// `/var` on one side and `/private/var` on the other, or a checkout reached
/// through a symbolic link.
///
/// A relative target is not ambiguous either: the operating system resolves it
/// against the directory holding the link, so `base` is that directory and the
/// answer is the one the kernel would reach. Declining to resolve it would not
/// be the conservative choice — the link would be left out of the plan, the
/// preview would call the skill uninstalled, and verification would then read
/// the revival as an undisclosed change after the write had already landed.
///
/// The final component is compared byte-for-byte, which is what every other
/// surface here does with a path component. On a case-insensitive filesystem
/// that misses a link differing only in case from the directory the update
/// creates; the answer is one comparison rule for the whole codebase rather
/// than filesystem semantics in this helper alone, and it is `skilled-jgu`.
fn absent_directory_key(
    base: Option<&Path>,
    directory: &Path,
) -> Option<(PathBuf, std::ffi::OsString)> {
    let mut resolved = if directory.is_absolute() {
        directory.to_path_buf()
    } else {
        base?.join(directory)
    };
    // The name a link points at may be a link of its own. `canonicalize` would
    // follow the whole chain in one step if the end of it existed, and the end
    // of this one is the directory the update has yet to create — so the chain
    // is walked by hand until it reaches a name that is not a link, which is
    // the name that actually does not exist. Stopping at the first hop would
    // key an agent's link on the alias it goes through rather than on the
    // directory it ends at, and the two sides would never meet.
    for _ in 0..MAX_TARGET_LINK_HOPS {
        let is_link = std::fs::symlink_metadata(&resolved)
            .is_ok_and(|metadata| metadata.file_type().is_symlink());
        if !is_link {
            break;
        }
        let next = std::fs::read_link(&resolved).ok()?;
        resolved = if next.is_absolute() {
            next
        } else {
            resolved.parent()?.join(next)
        };
    }
    let parent = resolved.parent()?.canonicalize().ok()?;
    Some((parent, resolved.file_name()?.to_owned()))
}

/// How far a dangling target is followed through links of its own before the
/// chain is treated as one this cannot describe. A cycle terminates here
/// rather than looping, and the caller then declines to predict a revival.
const MAX_TARGET_LINK_HOPS: usize = 16;

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
        write_attempted: bool,
        persistence_error: Option<String>,
    },
    StateUnavailable {
        apply_error: Option<String>,
        write_attempted: bool,
        refresh_error: String,
    },
    Failed(String),
}

pub fn apply_repository_update(plan: &RepositoryUpdatePlan) -> Result<()> {
    let checkout = validate_repository_update(plan)?;
    git::fast_forward((&checkout).into(), &plan.target_revision)
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
        Ok(checkout) => (
            git::fast_forward((&checkout).into(), &plan.target_revision),
            true,
        ),
        Err(error) => (Err(error), false),
    }
}

/// Prove the checkout is the one the plan was confirmed against, and return
/// the handle the merge must run through.
///
/// Every Git process here, and the fast-forward after it, is spawned through
/// the returned [`git::RepositoryHandle`], so all of them act on the one
/// directory that was pinned and identity-checked at the top — a checkout
/// moved aside and replaced between any two of these steps changes what the
/// pathname names, not what those processes read or write.
///
/// Two reads are not bound that way, and both are deliberate. The closing
/// [`git::RepositoryHandle::still_names_its_path`] re-reading is pathname-bound
/// by design: the plan the user confirmed stated a path, so a proven repository
/// that is no longer at that path is refused rather than written wherever it
/// went. A rename inside the gap after that re-reading loses only the refusal —
/// the write still lands in the proven repository, which is the inversion
/// `skilled-2k3.8.5.1` asked for.
///
/// The other read here is not a Git process at all: [`surviving_removal`]'s
/// worktree occupant walk. Handed the pinned handle, it descends from the held
/// directory descriptor with `openat(2)` and lists through descriptors too, so
/// the vacating-candidate loop below observes the same checkout as the bound
/// Git children — a checkout renamed aside and restored around the loop can no
/// longer clear the guard with a readable, vacant decoy (the skilled-lr8
/// window). On platforms other than Linux and macOS the walk re-resolves
/// the pathname as before, and the guard-order narrowing is what remains
/// there.
fn validate_repository_update(plan: &RepositoryUpdatePlan) -> Result<git::RepositoryHandle> {
    if plan.is_blocked() {
        return Err(crate::Error::SourceChangedAfterPreview);
    }
    let checkout = git::RepositoryHandle::open(&plan.path)
        .map_err(|_| crate::Error::SourceChangedAfterPreview)?;
    checkout_is_the_planned_repository(plan, &checkout)?;
    let target: git::GitTarget = (&checkout).into();
    // The preview's refusal is not evidence about now. Every guard below reads
    // objects, and a promisor remote configured since the preview would let
    // them fetch lazily on the way into a write.
    if git::repository_is_partial_clone(target)? {
        return Err(crate::Error::SourceChangedAfterPreview);
    }
    let head = git::head_state(target)?;
    if head.reference() != Some(plan.current_reference.as_str())
        || head.revision() != plan.current_revision
    {
        return Err(crate::Error::SourceChangedAfterPreview);
    }
    let upstream = git::upstream_of(target, &head)?;
    if upstream.as_ref().map(Upstream::tracking_ref) != Some(plan.upstream_ref.as_str())
        || upstream.as_ref().and_then(Upstream::revision) != Some(plan.target_revision.as_str())
        || !git::commit_exists(target, &plan.target_revision)?
    {
        return Err(crate::Error::SourceChangedAfterPreview);
    }
    let state = git::worktree_state(target)?;
    if state.tracked_dirty() || !state.worktree_dirty_known {
        return Err(crate::Error::SourceChangedAfterPreview);
    }
    if incoming_untracked_collision(&state, &plan.changed_files).is_some() {
        return Err(crate::Error::SourceChangedAfterPreview);
    }
    // The occupant check that cleared this plan's removals read the worktree at
    // preview time, and `incoming_untracked_collision` above deliberately says
    // nothing about deleted paths. A file or directory dropped into a vacating
    // candidate since then would leave it standing, so the dangling links the
    // user confirmed would not be what the write produces — asked again here,
    // against the worktree as it stands now.
    let deleted = deleted_paths(&plan.changed_files);
    for candidate in &plan.affected.vacating_candidates {
        if surviving_removal(target, &plan.target_revision, candidate, &deleted)?.is_some() {
            return Err(crate::Error::SourceChangedAfterPreview);
        }
    }
    // Asked last, so the statement on the screen stays true up to the write:
    // the guards above cannot be redirected, but the path the user agreed to
    // must still lead to the directory they are all bound to.
    checkout.still_names_its_path()?;
    Ok(checkout)
}

/// Whether the pinned checkout is the exact repository the plan was made
/// against: `plan.path` is not a symbolic link, not reached through one, and
/// the identity read *through the handle* matches the planned one — so the
/// answer describes the directory every later guard and the merge act on,
/// rather than whatever the pathname resolves to at each spawn.
fn checkout_is_the_planned_repository(
    plan: &RepositoryUpdatePlan,
    checkout: &git::RepositoryHandle,
) -> Result<()> {
    let metadata = std::fs::symlink_metadata(&plan.path)
        .map_err(|_| crate::Error::SourceChangedAfterPreview)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || plan.path.canonicalize().ok().as_deref() != Some(plan.path.as_path())
    {
        return Err(crate::Error::SourceChangedAfterPreview);
    }
    checkout.still_names_its_path()?;
    let identity = git::repository_git_dir(checkout.into())
        .ok()
        .and_then(|git_dir| repository_identity_from_git_dir(git_dir).ok());
    if identity.as_ref() != Some(&plan.repository_identity) {
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
    // Establish what is standing at the path before believing anything read
    // through it. The guards and the write are separate processes over a
    // pathname, so a checkout replaced in between would answer every question
    // below on behalf of a repository the plan was never about — a HEAD that
    // matches would then read as a pass for a write that landed elsewhere.
    match repository_identity(&plan.path) {
        Ok(identity) if identity == plan.repository_identity => {
            match git::head_state((&plan.path).into()) {
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
            match git::worktree_state((&plan.path).into()) {
                Ok(state) if state.tracked_dirty() => failures.push(
                    "the repository has tracked changes after the fast-forward operation".into(),
                ),
                Ok(state) if !state.worktree_dirty_known => withheld.push(
                    "post-update worktree cleanliness could not be determined without running configured filters"
                        .into(),
                ),
                Ok(_) => {}
                Err(error) => {
                    withheld.push(format!("worktree cleanliness could not be checked: {error}"));
                }
            }
        }
        Ok(_) => failures.push(format!(
            "{} is no longer the repository the update was planned against, so nothing read there answers for it",
            plan.path.display()
        )),
        Err(error) => withheld.push(format!(
            "the updated repository could not be identified, so its state was not checked: {error}"
        )),
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
        let expected_revival = plan
            .affected
            .expected_revival
            .iter()
            .map(|(name, agent, skill, path)| {
                ((name.as_str(), *agent), (skill.as_str(), path.as_path()))
            })
            .collect::<std::collections::BTreeMap<_, _>>();
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
                    // What was disclosed is that *this* link loses its target,
                    // so the link has to be the same one. A finding code alone
                    // does not say that: a disclosed hook can remove the link
                    // and leave a different dangling symlink under the same
                    // name, which carries `install.dangling_symlink` just as
                    // readily while having mutated a path outside the plan.
                    // The object carries the raw target, so comparing it is
                    // what distinguishes the two — the same test the disclosed
                    // restoration below already applies.
                    if !after_observation.is_some_and(|after| {
                        after.object() == observation.object()
                            && after
                                .findings()
                                .iter()
                                .any(|finding| finding.code() == "install.dangling_symlink")
                    }) {
                        failures.push(format!(
                            "the disclosed removal of {} was not observed as the same installation left dangling for {}",
                            row.name(),
                            observation.agent().display_name()
                        ));
                    }
                    continue;
                }
                // The disclosed counterpart of a removal: a link the plan said
                // this update would give a target to. What was disclosed is
                // that *this* link starts resolving, so the link has to be the
                // same one — the recorded target unchanged — and it has to
                // resolve into the updated source, no worse off than it was.
                // Accepting any improvement into the same source would let a
                // hook retarget the link at different content there and still
                // be reported as the restoration that was agreed to — so the
                // skill it resolves to has to be the one the plan named, not
                // merely something in the same repository. The link's own
                // target being unchanged does not settle that: the planner
                // follows intermediate aliases to find the destination, and
                // retargeting one of those leaves this link untouched.
                if let Some((expected_skill, expected_path)) =
                    expected_revival.get(&(row.name(), observation.agent().index()))
                {
                    match after_observation {
                        Some(after)
                            if after.object() == observation.object()
                                && after.health() <= observation.health()
                                && after.resolution().is_some_and(|variant| {
                                    variant.source_id() == plan.source_id
                                        && variant.variant_relative_path() == *expected_path
                                }) => {}
                        Some(_) => failures.push(format!(
                            "the disclosed restoration of {} for {} did not leave the link pointing where it did and resolving to {} in {}",
                            row.name(),
                            observation.agent().display_name(),
                            expected_skill,
                            plan.source_label
                        )),
                        None => failures.push(format!(
                            "installation {} for {} disappeared without disclosure",
                            row.name(),
                            observation.agent().display_name()
                        )),
                    }
                    continue;
                }
                // An installation this update disclosed nothing about. A
                // fast-forward writes inside the repository and nowhere near an
                // agent root, so the link is expected to come through
                // untouched — the *same* link, not merely one as healthy that
                // resolves as far. Health and resolution cannot tell the
                // difference: a disclosed hook can retarget the link at another
                // route to the same variant and satisfy both. The object
                // carries the raw target, which is what separates them, and the
                // disclosed removal and restoration branches above already
                // compare it. It is not only this report's honesty at stake —
                // repair proves ownership by comparing a link's raw target
                // against a receipt byte for byte, so a target rewritten here
                // and passed off as verified costs the link that evidence.
                match after_observation {
                    Some(after) if after.object() != observation.object() => {
                        failures.push(format!(
                            "installation {} for {} was retargeted without disclosure",
                            row.name(),
                            observation.agent().display_name()
                        ))
                    }
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
