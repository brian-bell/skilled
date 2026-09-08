//! One confirmed replacement, followed by independent filesystem and metadata
//! verification. Staging and retained old files are disclosed by the plan.

use super::*;
use crate::{
    git,
    provenance::{ManifestEntry, observe_directory_manifest},
};
use std::{
    fs,
    io::Read,
    sync::atomic::AtomicU64,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct RepositoryState {
    head: git::HeadState,
    git_dir: PathBuf,
    index: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LinkState {
    path: PathBuf,
    target: Option<std::ffi::OsString>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplyPlan {
    preview: Preview,
    staging: PathBuf,
    repository: Option<RepositoryState>,
    links: Vec<LinkState>,
    database: PathBuf,
    replacement_supported: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplyStatus {
    NoOp,
    Blocked,
    Partial,
    Verified,
    VerificationFailed,
    VerificationIncomplete,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplyOutcome {
    pub status: ApplyStatus,
    details: Vec<String>,
    metadata: MetadataAvailability,
}

impl ApplyOutcome {
    pub fn metadata_available(&self) -> bool {
        self.metadata == MetadataAvailability::Available
    }
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            match self.status {
                ApplyStatus::NoOp => "No changes were needed.",
                ApplyStatus::Blocked => "Update blocked before replacing skill files.",
                ApplyStatus::Partial => {
                    "Update partially applied; inspect retained paths before recovery."
                }
                ApplyStatus::Verified => "Skill update verified; provenance saved.",
                ApplyStatus::VerificationFailed => "Update written, but verification failed.",
                ApplyStatus::VerificationIncomplete => "Skill update verification is incomplete.",
            }
            .into(),
        ];
        lines.extend(self.details.clone());
        lines
    }
    pub(crate) fn unreported(confirmed_plan: Vec<String>) -> Self {
        let mut details = vec![
            "Replacement worker ended without a result. Files may have changed; inspect the source and retained recovery paths.".into(),
            "Confirmed plan follows; the outcome of each operation is unknown:".into(),
        ];
        details.extend(confirmed_plan);
        Self {
            status: ApplyStatus::VerificationIncomplete,
            details,
            metadata: MetadataAvailability::Available,
        }
    }
    #[cfg(test)]
    pub(crate) fn fixture() -> Self {
        Self {
            status: ApplyStatus::Verified,
            details: vec!["Source HEAD and index unchanged.".into()],
            metadata: MetadataAvailability::Available,
        }
    }
}

impl ApplyPlan {
    pub fn is_noop(&self) -> bool {
        self.preview.is_noop()
    }
    pub fn can_apply(&self) -> bool {
        self.replacement_supported && !self.is_noop()
    }
    pub fn lines(&self) -> Vec<String> {
        let mut lines = self.preview.lines();
        if self.is_noop() {
            lines.push("Skill is up to date; no replacement is needed.".into());
            return lines;
        }
        if !self.replacement_supported {
            lines.push(
                "Read-only preview: guarded replacement is available only on Linux and macOS."
                    .into(),
            );
            return lines;
        }
        lines[0] =
            "Confirming replaces only this skill's planned files and advances its provenance."
                .into();
        lines.push(format!(
            "Private staging and retained old files: {}",
            self.staging.display()
        ));
        for entry in &self.preview.expected_entries {
            lines.push(format!(
                "Stage {:?}: {}",
                entry.kind,
                self.staging.join(&entry.relative_path).display()
            ));
        }
        if let Some(request) = self.preview.request.as_deref() {
            for entry in &self.preview.expected_entries {
                if entry.kind == ManifestEntryKind::Directory
                    && !request
                        .local
                        .entries
                        .iter()
                        .any(|old| old.relative_path == entry.relative_path)
                {
                    lines.push(format!(
                        "Create directory: {}",
                        self.preview.skill.join(&entry.relative_path).display()
                    ));
                }
            }
            for entry in &request.local.entries {
                if entry.kind == ManifestEntryKind::File
                    && !self
                        .preview
                        .expected_entries
                        .iter()
                        .any(|new| new.relative_path == entry.relative_path)
                {
                    lines.push(format!(
                        "Retain removed file at: {}",
                        replacement::removed_backup_path(&self.staging, &entry.relative_path)
                            .display()
                    ));
                }
            }
        }
        lines.push(format!("Provenance database: {}", self.database.display()));
        lines.push(format!(
            "Record verified origin revision: {}",
            self.preview.revision
        ));
        lines.push(
            "Old files remain in the disclosed staging directory; no recursive cleanup runs."
                .into(),
        );
        lines.push("The containing checkout remains uncommitted; HEAD, index, and installation links must stay unchanged.".into());
        lines
    }
    #[cfg(test)]
    pub(crate) fn readonly_fixture() -> Self {
        let mut plan = Self::fixture();
        plan.replacement_supported = false;
        plan
    }
    #[cfg(test)]
    pub(crate) fn fixture() -> Self {
        Self {
            preview: Preview::fixture(),
            staging: "/source/.skilled-stage-demo".into(),
            repository: None,
            links: vec![],
            database: "/data/skilled.sqlite3".into(),
            replacement_supported: true,
        }
    }
}

pub(crate) fn plan_apply(preview: &Preview, data_dir: &Path) -> Result<ApplyPlan, AdoptionFailure> {
    let request = preview
        .request
        .as_deref()
        .ok_or("Preview has no guarded check context")?;
    recheck_request(request, data_dir)?;
    recheck_affected_installations(request, data_dir)?;
    let replacement_supported = cfg!(any(target_os = "linux", target_os = "macos"));
    if preview.is_noop() || !replacement_supported {
        return Ok(ApplyPlan {
            preview: preview.clone(),
            staging: PathBuf::new(),
            repository: None,
            links: vec![],
            database: data_dir.join("skilled.sqlite3"),
            replacement_supported,
        });
    }
    let repository = repository_state(&request.checkout)?;
    let handle =
        git::RepositoryHandle::open(&request.checkout).map_err(|error| error.to_string())?;
    let state = git::worktree_state((&handle).into()).map_err(|error| error.to_string())?;
    if state.tracked_dirty() || !state.worktree_dirty_known {
        return Err(
            "Repository tracked content or index is modified or could not be proven clean".into(),
        );
    }
    if repository.head.reference().is_none() {
        return Err("Repository HEAD is detached".into());
    }
    let links = link_states(&request.affected_installations)?;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    // Keep retained SKILL.md files outside the registered checkout so they
    // cannot become catalog candidates on the mandatory post-apply rescan.
    let staging = request
        .checkout
        .parent()
        .ok_or("Selected source has no staging parent")?
        .join(format!(
            ".skilled-update-{stamp}-{}",
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    Ok(ApplyPlan {
        preview: preview.clone(),
        staging,
        repository: Some(repository),
        links,
        database: data_dir.join("skilled.sqlite3"),
        replacement_supported,
    })
}

fn link_states(installations: &[AffectedInstallation]) -> Result<Vec<LinkState>, AdoptionFailure> {
    installations
        .iter()
        .map(|installation| {
            let metadata =
                fs::symlink_metadata(&installation.path).map_err(|error| error.to_string())?;
            let target = if metadata.file_type().is_symlink() {
                Some(
                    fs::read_link(&installation.path)
                        .map_err(|error| error.to_string())?
                        .into_os_string(),
                )
            } else if metadata.is_dir() {
                None
            } else {
                return Err("Affected installation is no longer a directory or link".into());
            };
            Ok(LinkState {
                path: installation.path.clone(),
                target,
            })
        })
        .collect()
}

fn repository_state(checkout: &Path) -> Result<RepositoryState, AdoptionFailure> {
    let handle = git::RepositoryHandle::open(checkout).map_err(|error| error.to_string())?;
    let head = git::head_state((&handle).into()).map_err(|error| error.to_string())?;
    let git_dir = git::repository_git_dir((&handle).into()).map_err(|error| error.to_string())?;
    for marker in [
        "MERGE_HEAD",
        "REBASE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "index.lock",
    ] {
        match fs::symlink_metadata(git_dir.join(marker)) {
            Ok(_) => {
                return Err(
                    format!("Repository operation marker blocks replacement: {marker}").into(),
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string().into()),
        }
    }
    let path = git_dir.join("index");
    let index = match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string().into()),
        Ok(metadata) => {
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() > 32 * 1024 * 1024
            {
                return Err("Repository index is unsafe or exceeds its read budget".into());
            }
            let mut bytes = Vec::new();
            let mut options = fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
            }
            options
                .open(&path)
                .map_err(|error| error.to_string())?
                .take(32 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            if bytes.len() > 32 * 1024 * 1024 {
                return Err("Repository index exceeds its read budget".into());
            }
            Some(bytes)
        }
    };
    handle
        .still_names_its_path()
        .map_err(|error| error.to_string())?;
    Ok(RepositoryState {
        head,
        git_dir,
        index,
    })
}

fn stable_guards(plan: &ApplyPlan, request: &CheckRequest) -> Result<(), AdoptionFailure> {
    if adoption::directories(&request.checkout, request.variant.variant_relative_path())
        .map_err(observation_failure)?
        != request.directories
    {
        return Err("Selected skill ancestors changed after preview".into());
    }
    if crate::source::repository_identity(&request.checkout).map_err(|error| error.to_string())?
        != request.identity
    {
        return Err("Selected source identity changed after preview".into());
    }
    if Some(repository_state(&request.checkout)?) != plan.repository {
        return Err("Repository HEAD, index, or operation state changed after preview".into());
    }
    if link_states(&request.affected_installations)? != plan.links {
        return Err("Installation links changed after preview".into());
    }
    Ok(())
}

pub(crate) fn apply(plan: &ApplyPlan, data_dir: &Path) -> ApplyOutcome {
    let mut outcome = ApplyOutcome {
        status: ApplyStatus::Blocked,
        details: Vec::new(),
        metadata: MetadataAvailability::Available,
    };
    let result = Store::open(data_dir)
        .map_err(metadata_failure)
        .and_then(|mut store| apply_inner(plan, data_dir, &mut outcome, &mut store));
    if let Err(failure) = result {
        outcome.metadata = failure.metadata;
        outcome.details.push(failure.message);
    }
    outcome
}

fn apply_inner(
    plan: &ApplyPlan,
    data_dir: &Path,
    outcome: &mut ApplyOutcome,
    store: &mut Store,
) -> Result<(), AdoptionFailure> {
    let request = plan
        .preview
        .request
        .as_deref()
        .ok_or("Preview has no guarded check context")?;
    if plan.database != data_dir.join("skilled.sqlite3") {
        return Err("Application metadata location changed after preview".into());
    }
    let mutation = store.begin_mutation().map_err(metadata_failure)?;
    if !mutation
        .variant_registration_matches(&request.variant, &request.checkout)
        .map_err(metadata_failure)?
    {
        return Err("Variant registration changed after preview".into());
    }
    recheck_under_guard(request, &mutation)?;
    if plan.is_noop() {
        outcome.status = ApplyStatus::NoOp;
        return Ok(());
    }
    if !plan.can_apply() {
        return Err("Guarded replacement is unavailable on this platform".into());
    }
    stable_guards(plan, request)?;
    clean_worktree(request)?;
    let expected = DirectoryManifest {
        baseline: plan.preview.proposed_baseline.clone(),
        entries: plan
            .preview
            .expected_entries
            .iter()
            .map(|entry| ManifestEntry {
                relative_path: entry.relative_path.clone(),
                kind: entry.kind,
                executable: entry.executable,
                bytes: entry.bytes.clone(),
            })
            .collect(),
    };
    let mut stage = replacement::Stage::prepare(&plan.preview.skill, &plan.staging, &expected)
        .map_err(AdoptionFailure::from)?;
    outcome
        .details
        .push(format!("Staging retained at {}", plan.staging.display()));
    recheck_under_guard(request, &mutation)?;
    stable_guards(plan, request)?;
    clean_worktree(request)?;
    let report = stage.apply_with_guard(&request.local, || {
        stable_guards(plan, request).map_err(|error| error.message)
    });
    if report.changed() {
        outcome.status = ApplyStatus::Partial;
    }
    outcome.details.extend(report.lines());
    // Every write attempt gets a fresh inventory/catalog scan, even when the
    // executor stopped early. Its result cannot erase the partial outcome.
    if report.success() {
        outcome.status = ApplyStatus::VerificationIncomplete;
    }
    let rescan = mutation
        .registered_sources_leaving_uninspected(&request.checkout)
        .map_err(metadata_failure)
        .and_then(|sources| verify_installations(request, &sources, outcome));
    if !report.success() {
        rescan?;
        return Ok(());
    }
    let actual = observe_directory_manifest(&plan.preview.skill).map_err(|error| {
        if matches!(error, crate::provenance::ObservationFailure::Changed(_)) {
            outcome.status = ApplyStatus::VerificationFailed;
        }
        observation_failure(error)
    })?;
    if actual != expected {
        outcome.status = ApplyStatus::VerificationFailed;
        return Err("Materialized skill differs from the confirmed expected manifest".into());
    }
    stable_guards(plan, request)
        .inspect_err(|_| outcome.status = ApplyStatus::VerificationFailed)?;
    rescan?;
    outcome.status = ApplyStatus::Partial;
    mutation
        .advance_origin(&request.record, &expected.baseline, &plan.preview.revision)
        .map_err(metadata_failure)?;
    mutation.commit().map_err(metadata_failure)?;
    outcome.status = ApplyStatus::VerificationIncomplete;
    let mut expected_record = request.record.clone();
    expected_record.baseline = expected.baseline.clone();
    expected_record.proven_revision = Some(plan.preview.revision.clone());
    if store
        .origin_record(
            request.variant.source_id(),
            request.variant.catalog_relative_path(),
            request.variant.variant_relative_path(),
        )
        .map_err(metadata_failure)?
        != Some(expected_record)
    {
        outcome.status = ApplyStatus::VerificationFailed;
        return Err("Saved provenance differs from the verified replacement".into());
    }
    stable_guards(plan, request)
        .inspect_err(|_| outcome.status = ApplyStatus::VerificationFailed)?;
    let sources = store
        .registered_sources_leaving_uninspected(&request.checkout)
        .map_err(metadata_failure)?;
    verify_installations(request, &sources, outcome)?;
    let verified = observe_directory_manifest(&plan.preview.skill).map_err(|error| {
        if matches!(error, crate::provenance::ObservationFailure::Changed(_)) {
            outcome.status = ApplyStatus::VerificationFailed;
        }
        observation_failure(error)
    })?;
    if verified != expected {
        outcome.status = ApplyStatus::VerificationFailed;
        return Err("Skill changed after provenance was saved".into());
    }
    outcome.status = ApplyStatus::Verified;
    Ok(())
}

fn verify_installations(
    request: &CheckRequest,
    sources: &[RegisteredSource],
    outcome: &mut ApplyOutcome,
) -> Result<(), AdoptionFailure> {
    inspect_installations_from_sources(request, sources).map_err(|failure| {
        if matches!(failure, InstallationRecheckFailure::Changed)
            && outcome.status == ApplyStatus::VerificationIncomplete
        {
            outcome.status = ApplyStatus::VerificationFailed;
        }
        failure.into()
    })
}

fn clean_worktree(request: &CheckRequest) -> Result<(), AdoptionFailure> {
    let handle =
        git::RepositoryHandle::open(&request.checkout).map_err(|error| error.to_string())?;
    let state = git::worktree_state((&handle).into()).map_err(|error| error.to_string())?;
    if state.tracked_dirty() || !state.worktree_dirty_known {
        return Err("Repository tracked content or index changed before replacement".into());
    }
    handle
        .still_names_its_path()
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn rescan_under_guard(
    request: &CheckRequest,
    mutation: &crate::store::Mutation<'_>,
) -> Result<(), AdoptionFailure> {
    let sources = mutation
        .registered_sources_leaving_uninspected(&request.checkout)
        .map_err(metadata_failure)?;
    if !sources
        .iter()
        .any(|source| registration_matches(source, &request.source, &request.variant))
    {
        return Err("Selected source registration changed after preview".into());
    }
    recheck_installations_from_sources(request, &sources)
}

fn recheck_under_guard(
    request: &CheckRequest,
    mutation: &crate::store::Mutation<'_>,
) -> Result<(), AdoptionFailure> {
    if !mutation
        .origin_records(request.variant.source_id())
        .map_err(metadata_failure)?
        .contains(&request.record)
    {
        return Err("Adopted origin record changed after preview".into());
    }
    rescan_under_guard(request, mutation)?;
    let local =
        adoption::observe_variant_manifest(&request.checkout, &request.variant, &request.identity)
            .map_err(observation_failure)?;
    if local != request.local {
        return Err("Vendored skill content changed after preview".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn installation_verification_distinguishes_changed_from_incomplete() {
        let (temporary, store, preview) = super::super::tests::apply_fixture();
        let request = preview.request.as_deref().unwrap();
        let root = temporary
            .path()
            .join("home")
            .join(crate::agents::adapter(crate::AgentKind::Codex).native_skill_root());
        fs::create_dir_all(root.parent().unwrap()).unwrap();
        fs::write(&root, "not a readable root").unwrap();
        let sources = store.registered_sources().unwrap();
        let mut outcome = ApplyOutcome {
            status: ApplyStatus::VerificationIncomplete,
            details: vec![],
            metadata: MetadataAvailability::Available,
        };
        assert!(verify_installations(request, &sources, &mut outcome).is_err());
        assert_eq!(outcome.status, ApplyStatus::VerificationIncomplete);
        fs::remove_file(&root).unwrap();
        fs::create_dir_all(&root).unwrap();
        std::os::unix::fs::symlink(&preview.skill, root.join("alias")).unwrap();
        assert!(verify_installations(request, &sources, &mut outcome).is_err());
        assert_eq!(outcome.status, ApplyStatus::VerificationFailed);
    }

    #[test]
    fn metadata_failure_after_files_changed_is_partial_and_keeps_old_record() {
        let (temporary, mut store, preview) = super::super::tests::apply_fixture();
        let data = temporary.path().join("data");
        let plan = plan_apply(&preview, &data).unwrap();
        store.fail_next(crate::store::MetadataOperation::AdvanceOrigin);
        let mut outcome = ApplyOutcome {
            status: ApplyStatus::Blocked,
            details: vec![],
            metadata: MetadataAvailability::Available,
        };
        let failure = apply_inner(&plan, &data, &mut outcome, &mut store).unwrap_err();
        assert_eq!(outcome.status, ApplyStatus::Partial);
        assert_ne!(failure.metadata, MetadataAvailability::Available);
        let request = preview.request.as_deref().unwrap();
        assert_eq!(
            store.origin_records(request.variant.source_id()).unwrap(),
            vec![request.record.clone()]
        );
        assert_eq!(
            observe_directory_manifest(&preview.skill).unwrap().baseline,
            preview.proposed_baseline
        );
        assert!(plan.staging.join("SKILL.md").exists());
        assert_eq!(
            fs::read(plan.staging.join("SKILL.md")).unwrap(),
            request
                .local
                .entries
                .iter()
                .find(|e| e.relative_path == Path::new("SKILL.md"))
                .unwrap()
                .bytes
        );
    }
}
