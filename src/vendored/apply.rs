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
    head_file: Vec<u8>,
    git_dir: PathBuf,
    index: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LinkState {
    path: PathBuf,
    target: Option<std::ffi::OsString>,
}

enum StableGuardFailure {
    Changed(AdoptionFailure),
    Unavailable(AdoptionFailure),
}

impl StableGuardFailure {
    fn io(error: std::io::Error) -> Self {
        match crate::provenance::ObservationFailure::io(error.to_string(), error) {
            crate::provenance::ObservationFailure::Changed(message) => {
                Self::Changed(message.into())
            }
            crate::provenance::ObservationFailure::Unavailable(message) => {
                Self::Unavailable(message.into())
            }
        }
    }

    fn into_failure(self) -> AdoptionFailure {
        match self {
            Self::Changed(failure) | Self::Unavailable(failure) => failure,
        }
    }
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
                    && !request.local.entries.iter().any(|old| {
                        old.relative_path == entry.relative_path
                            && old.kind == ManifestEntryKind::Directory
                    })
                {
                    lines.push(format!(
                        "Create directory: {}",
                        self.preview.skill.join(&entry.relative_path).display()
                    ));
                }
            }
            for entry in &request.local.entries {
                if entry.kind == ManifestEntryKind::File
                    && !self.preview.expected_entries.iter().any(|new| {
                        new.relative_path == entry.relative_path
                            && new.kind == ManifestEntryKind::File
                    })
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
    let repository =
        repository_state(&request.checkout).map_err(StableGuardFailure::into_failure)?;
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
    let links =
        link_states(&request.affected_installations).map_err(StableGuardFailure::into_failure)?;
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

fn link_states(
    installations: &[AffectedInstallation],
) -> Result<Vec<LinkState>, StableGuardFailure> {
    installations
        .iter()
        .map(|installation| {
            let metadata =
                fs::symlink_metadata(&installation.path).map_err(StableGuardFailure::io)?;
            let target = if metadata.file_type().is_symlink() {
                Some(
                    fs::read_link(&installation.path)
                        .map_err(|error| {
                            // read_link rejects a path that became a regular object.
                            if error.kind() == std::io::ErrorKind::InvalidInput {
                                StableGuardFailure::Changed(error.to_string().into())
                            } else {
                                StableGuardFailure::io(error)
                            }
                        })?
                        .into_os_string(),
                )
            } else if metadata.is_dir() {
                None
            } else {
                return Err(StableGuardFailure::Changed(
                    "Affected installation is no longer a directory or link".into(),
                ));
            };
            Ok(LinkState {
                path: installation.path.clone(),
                target,
            })
        })
        .collect()
}

fn repository_state(checkout: &Path) -> Result<RepositoryState, StableGuardFailure> {
    let handle = git::RepositoryHandle::open(checkout)
        .map_err(|error| StableGuardFailure::Unavailable(error.to_string().into()))?;
    // A Git command failure alone does not distinguish an absent/corrupt ref
    // from unreadable objects or metadata. Directly observed path/HEAD/index
    // changes are classified separately; do not infer disagreement from stderr.
    let head = git::head_state((&handle).into())
        .map_err(|error| StableGuardFailure::Unavailable(error.to_string().into()))?;
    let git_dir = git::repository_git_dir((&handle).into())
        .map_err(|error| StableGuardFailure::Unavailable(error.to_string().into()))?;
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
                return Err(StableGuardFailure::Changed(
                    format!("Repository operation marker blocks replacement: {marker}").into(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(StableGuardFailure::io(error));
            }
        }
    }
    let path = git_dir.join("index");
    let index = match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(StableGuardFailure::io(error)),
        Ok(metadata) => {
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() > 32 * 1024 * 1024
            {
                return Err(StableGuardFailure::Changed(
                    "Repository index is unsafe or exceeds its read budget".into(),
                ));
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
                .map_err(StableGuardFailure::io)?
                .take(32 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(StableGuardFailure::io)?;
            if bytes.len() > 32 * 1024 * 1024 {
                return Err(StableGuardFailure::Changed(
                    "Repository index exceeds its read budget".into(),
                ));
            }
            Some(bytes)
        }
    };
    handle
        .still_names_its_path()
        .map_err(|error| StableGuardFailure::Unavailable(error.to_string().into()))?;
    Ok(RepositoryState {
        head_file: read_head_file(&git_dir)?,
        head,
        git_dir,
        index,
    })
}

fn read_head_file(git_dir: &Path) -> Result<Vec<u8>, StableGuardFailure> {
    let path = git_dir.join("HEAD");
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(StableGuardFailure::io)?;
    let metadata = file.metadata().map_err(StableGuardFailure::io)?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        return Err(StableGuardFailure::Changed(
            "Repository HEAD is unsafe or exceeds its read budget".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(StableGuardFailure::io)?;
    if bytes.len() > 64 * 1024 {
        return Err(StableGuardFailure::Changed(
            "Repository HEAD exceeds its read budget".into(),
        ));
    }
    Ok(bytes)
}

fn stable_guards(plan: &ApplyPlan, request: &CheckRequest) -> Result<(), StableGuardFailure> {
    if adoption::directories(&request.checkout, request.variant.variant_relative_path()).map_err(
        |failure| match failure {
            crate::provenance::ObservationFailure::Changed(message) => {
                StableGuardFailure::Changed(message.into())
            }
            crate::provenance::ObservationFailure::Unavailable(message) => {
                StableGuardFailure::Unavailable(message.into())
            }
        },
    )? != request.directories
    {
        return Err(StableGuardFailure::Changed(
            "Selected skill ancestors changed after preview".into(),
        ));
    }
    // Observe the confirmed Git directory directly before invoking Git: a
    // missing or redirected directory is evidence of change, even when Git
    // subsequently reports only a generic command failure.
    if let Some(repository) = &plan.repository {
        let metadata = fs::symlink_metadata(&repository.git_dir).map_err(StableGuardFailure::io)?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || repository
                .git_dir
                .canonicalize()
                .map_err(StableGuardFailure::io)?
                != repository.git_dir
        {
            return Err(StableGuardFailure::Changed(
                "Confirmed Git directory changed after preview".into(),
            ));
        }
    }
    if let Some(repository) = &plan.repository
        && read_head_file(&repository.git_dir)? != repository.head_file
    {
        return Err(StableGuardFailure::Changed(
            "Repository HEAD file changed after preview".into(),
        ));
    }
    if crate::source::repository_identity(&request.checkout).map_err(|error| match error {
        crate::Error::Io(error) => StableGuardFailure::io(error),
        error => StableGuardFailure::Unavailable(error.to_string().into()),
    })? != request.identity
    {
        return Err(StableGuardFailure::Changed(
            "Selected source identity changed after preview".into(),
        ));
    }
    let current_repository = repository_state(&request.checkout)?;
    if Some(current_repository) != plan.repository {
        return Err(StableGuardFailure::Changed(
            "Repository HEAD, index, or operation state changed after preview".into(),
        ));
    }
    let current_links = link_states(&request.affected_installations)?;
    if current_links != plan.links {
        return Err(StableGuardFailure::Changed(
            "Installation links changed after preview".into(),
        ));
    }
    Ok(())
}

fn postwrite_guards(
    plan: &ApplyPlan,
    request: &CheckRequest,
    outcome: &mut ApplyOutcome,
) -> Result<(), AdoptionFailure> {
    classify_postwrite_guards(stable_guards(plan, request), outcome)
}

fn classify_postwrite_guards(
    result: Result<(), StableGuardFailure>,
    outcome: &mut ApplyOutcome,
) -> Result<(), AdoptionFailure> {
    match result {
        Ok(()) => Ok(()),
        Err(StableGuardFailure::Changed(failure)) => {
            outcome.status = ApplyStatus::VerificationFailed;
            Err(failure)
        }
        Err(StableGuardFailure::Unavailable(failure)) => Err(failure),
    }
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
    stable_guards(plan, request).map_err(StableGuardFailure::into_failure)?;
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
    stable_guards(plan, request).map_err(StableGuardFailure::into_failure)?;
    clean_worktree(request)?;
    let report = stage.apply_with_guard(&request.local, || {
        stable_guards(plan, request).map_err(|error| error.into_failure().message)
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
    postwrite_guards(plan, request, outcome)?;
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
    postwrite_guards(plan, request, outcome)?;
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
        AdoptionFailure::from(failure)
    })?;
    recheck_unique_name(request, sources).inspect_err(|_| {
        if outcome.status == ApplyStatus::VerificationIncomplete {
            outcome.status = ApplyStatus::VerificationFailed;
        }
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
    recheck_installations_from_sources(request, &sources)?;
    recheck_unique_name(request, &sources)
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

    #[cfg(unix)]
    #[test]
    fn missing_installation_is_a_failed_postwrite_guard() {
        let (temporary, _store, mut preview) = super::super::tests::apply_fixture();
        let link = temporary
            .path()
            .join("home")
            .join(crate::agents::adapter(crate::AgentKind::Codex).native_skill_root())
            .join("demo");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&preview.skill, &link).unwrap();
        preview
            .request
            .as_mut()
            .unwrap()
            .affected_installations
            .push(AffectedInstallation {
                path: link.clone(),
                agent: crate::AgentKind::Codex,
            });
        let plan = plan_apply(&preview, &temporary.path().join("data")).unwrap();
        fs::remove_file(&link).unwrap();
        let mut outcome = ApplyOutcome {
            status: ApplyStatus::VerificationIncomplete,
            details: vec![],
            metadata: MetadataAvailability::Available,
        };
        assert!(
            postwrite_guards(&plan, preview.request.as_deref().unwrap(), &mut outcome).is_err()
        );
        assert_eq!(outcome.status, ApplyStatus::VerificationFailed);
        assert!(matches!(
            StableGuardFailure::io(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            StableGuardFailure::Unavailable(_)
        ));
    }

    #[test]
    fn missing_or_corrupted_head_is_failed_verification() {
        for corrupt in [false, true] {
            let (temporary, _store, preview) = super::super::tests::apply_fixture();
            let plan = plan_apply(&preview, &temporary.path().join("data")).unwrap();
            let head = plan.repository.as_ref().unwrap().git_dir.join("HEAD");
            if corrupt {
                fs::write(&head, "invalid HEAD").unwrap();
            } else {
                fs::remove_file(&head).unwrap();
            }
            let mut outcome = ApplyOutcome {
                status: ApplyStatus::VerificationIncomplete,
                details: vec![],
                metadata: MetadataAvailability::Available,
            };
            assert!(
                postwrite_guards(&plan, preview.request.as_deref().unwrap(), &mut outcome).is_err()
            );
            assert_eq!(outcome.status, ApplyStatus::VerificationFailed);
        }
    }

    #[test]
    fn disappeared_or_replaced_git_directory_is_failed_verification() {
        for replaced in [false, true] {
            let (temporary, _store, preview) = super::super::tests::apply_fixture();
            let plan = plan_apply(&preview, &temporary.path().join("data")).unwrap();
            let git_dir = &plan.repository.as_ref().unwrap().git_dir;
            fs::rename(git_dir, temporary.path().join("retained-git")).unwrap();
            if replaced {
                fs::write(git_dir, "not a Git directory").unwrap();
            }
            let mut outcome = ApplyOutcome {
                status: ApplyStatus::VerificationIncomplete,
                details: vec![],
                metadata: MetadataAvailability::Available,
            };
            assert!(
                postwrite_guards(&plan, preview.request.as_deref().unwrap(), &mut outcome).is_err()
            );
            assert_eq!(outcome.status, ApplyStatus::VerificationFailed);
        }
    }

    #[test]
    fn file_to_directory_preview_discloses_the_hashed_recovery_path() {
        let (temporary, _store, preview) = super::super::tests::apply_fixture();
        let data = temporary.path().join("data");
        let mut plan = plan_apply(&preview, &data).expect("live plan");
        let request = plan.preview.request.as_mut().expect("request");
        request.local.entries.push(ManifestEntry {
            relative_path: PathBuf::from("assets"),
            kind: ManifestEntryKind::File,
            executable: false,
            bytes: b"old file\n".to_vec(),
        });
        plan.preview.expected_entries.push(ExpectedEntry {
            relative_path: PathBuf::from("assets"),
            kind: ManifestEntryKind::Directory,
            executable: false,
            bytes: Vec::new(),
        });
        let recovery = replacement::removed_backup_path(&plan.staging, Path::new("assets"));
        assert!(
            plan.lines()
                .iter()
                .any(|line| line == &format!("Retain removed file at: {}", recovery.display()))
        );
        assert!(plan.lines().iter().any(|line| line
            == &format!(
                "Create directory: {}",
                plan.preview.skill.join("assets").display()
            )));
    }

    #[cfg(unix)]
    #[test]
    fn postwrite_guards_classify_live_marker_and_unavailable_observations() {
        let (temporary, _store, preview) = super::super::tests::apply_fixture();
        let data = temporary.path().join("data");
        let plan = plan_apply(&preview, &data).expect("live plan");
        let request = plan.preview.request.as_deref().expect("request");
        let git_dir = plan
            .repository
            .as_ref()
            .expect("repository state")
            .git_dir
            .clone();

        let marker = git_dir.join("MERGE_HEAD");
        fs::write(&marker, "in progress\n").expect("operation marker");
        let changed = stable_guards(&plan, request);
        fs::remove_file(&marker).expect("remove operation marker");
        assert!(matches!(changed, Err(StableGuardFailure::Changed(_))));
        let mut changed_outcome = ApplyOutcome {
            status: ApplyStatus::VerificationIncomplete,
            details: vec![],
            metadata: MetadataAvailability::Available,
        };
        assert!(classify_postwrite_guards(changed, &mut changed_outcome).is_err());
        assert_eq!(changed_outcome.status, ApplyStatus::VerificationFailed);

        use std::os::unix::fs::PermissionsExt;
        let original_permissions = fs::metadata(&git_dir)
            .expect("git directory metadata")
            .permissions();
        let mut unavailable_permissions = original_permissions.clone();
        unavailable_permissions.set_mode(0o000);
        fs::set_permissions(&git_dir, unavailable_permissions).expect("hide git directory");
        let unavailable = stable_guards(&plan, request);
        fs::set_permissions(&git_dir, original_permissions).expect("restore git directory");
        assert!(matches!(
            unavailable,
            Err(StableGuardFailure::Unavailable(_))
        ));
        let mut unavailable_outcome = ApplyOutcome {
            status: ApplyStatus::VerificationIncomplete,
            details: vec![],
            metadata: MetadataAvailability::Available,
        };
        assert!(classify_postwrite_guards(unavailable, &mut unavailable_outcome).is_err());
        assert_eq!(
            unavailable_outcome.status,
            ApplyStatus::VerificationIncomplete
        );
    }
}
