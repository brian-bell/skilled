//! Explicit, metadata-only adoption of one registered skill variant.
//!
//! Evidence suggests an origin; only the separately confirmed preview associates
//! it. Current bytes establish future comparisons, never historical provenance.
use std::{
    fs,
    path::{Component, Path, PathBuf},
    time::SystemTime,
};

use crate::{
    provenance::{
        Baseline, Evidence, ObservationFailure, Origin, observe_directory_hash, read_evidence,
    },
    resolution::VariantRef,
    source::{RegisteredSource, RepositoryIdentity, repository_identity},
    store::Store,
};

/// Whether the session can continue using its private metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataAvailability {
    Available,
    Unavailable,
}

/// A refusal before saving, or the reason a saved baseline is unverified.
/// Persistence is carried by `AdoptionPrompt`, never inferred from this text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdoptionFailure {
    pub message: String,
    pub metadata: MetadataAvailability,
}
impl From<String> for AdoptionFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            metadata: MetadataAvailability::Available,
        }
    }
}
impl From<&str> for AdoptionFailure {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}
impl std::fmt::Display for AdoptionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}
impl AdoptionFailure {
    pub(crate) fn metadata(error: crate::Error) -> Self {
        Self {
            metadata: if matches!(error, crate::Error::SourceChangedAfterPreview) {
                MetadataAvailability::Available
            } else {
                MetadataAvailability::Unavailable
            },
            message: error.to_string(),
        }
    }
}

/// Verification of a baseline whose metadata transaction has committed.
/// A failed observation differs from one we could not finish reading.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdoptionVerification {
    Verified,
    Failed(AdoptionFailure),
    Incomplete(AdoptionFailure),
}
impl AdoptionVerification {
    pub fn failure(&self) -> Option<&AdoptionFailure> {
        match self {
            Self::Verified => None,
            Self::Failed(failure) | Self::Incomplete(failure) => Some(failure),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OriginRecord {
    pub source_id: i64,
    pub catalog_relative_path: PathBuf,
    pub variant_relative_path: PathBuf,
    pub origin: Origin,
    pub update_ref: String,
    pub baseline: Baseline,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdoptionDraft {
    pub fields: [String; 3],
    pub focused: usize,
    pub evidence: Evidence,
    pub error: Option<String>,
    pub(crate) variant: VariantRef,
    pub(crate) checkout: PathBuf,
    pub(crate) identity: RepositoryIdentity,
}

impl AdoptionDraft {
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!(
                "Skill: {}",
                self.checkout
                    .join(self.variant.variant_relative_path())
                    .display()
            ),
            "Confirm an exact origin and tracking branch. Use . for the repository root.".into(),
            "Attribution and lock entries are hints, not proof of a historical revision.".into(),
        ];
        if self.evidence.is_ambiguous() {
            lines.push(
                "Ambiguous evidence: enter one exact origin to resolve it before previewing."
                    .into(),
            );
        }
        for origin in &self.evidence.candidates {
            lines.push(format!(
                "Hint: {} · {}",
                origin.repository,
                origin.subdirectory.as_deref().unwrap_or_default()
            ));
        }
        for problem in &self.evidence.problems {
            lines.push(format!("Evidence: {problem}"));
        }
        for (i, label) in [
            "Repository URL",
            "Subdirectory",
            "Tracking branch (refs/heads/…)",
        ]
        .iter()
        .enumerate()
        {
            lines.push(format!(
                "{} {label}: {}",
                if self.focused == i { ">" } else { " " },
                self.fields[i]
            ));
        }
        if let Some(error) = &self.error {
            lines.push(format!("Blocked: {error}"));
        }
        lines
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdoptionPlan {
    pub(crate) record: OriginRecord,
    pub(crate) variant: VariantRef,
    pub(crate) checkout: PathBuf,
    pub(crate) database: PathBuf,
    observation: AdoptionObservation,
    pub(crate) identity: RepositoryIdentity,
}

impl AdoptionPlan {
    pub fn lines(&self) -> Vec<String> {
        vec![
            "Adopt current content for future comparisons only.".into(),
            "No historical origin revision is proven or recorded.".into(),
            format!("Source: {}", self.checkout.display()),
            format!(
                "Catalog: {}",
                self.checkout
                    .join(&self.record.catalog_relative_path)
                    .display()
            ),
            format!(
                "Skill: {}",
                self.checkout
                    .join(&self.record.variant_relative_path)
                    .display()
            ),
            format!("Origin: {}", self.record.origin.repository()),
            format!("Origin subdirectory: {}", self.record.origin.subdirectory()),
            format!("Tracking branch: {}", self.record.update_ref),
            format!(
                "Baseline v{}: {}",
                self.record.baseline.version, self.record.baseline.digest
            ),
            format!(
                "Write origin association and baseline to: {}",
                self.database.display()
            ),
            "Skill files, attribution, lockfiles, and Git state remain unchanged.".into(),
            "Content, evidence, paths, and registration are rechecked before saving.".into(),
        ]
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdoptionPrompt {
    Editing(AdoptionDraft),
    Preview(AdoptionPlan),
    /// The transaction committed; verification may still have failed or be incomplete.
    Report(AdoptionVerification),
    /// The operation saved nothing. Also used for draft/preview failures.
    Failed(AdoptionFailure),
}

// Directory identities protect a same-content directory replacement between
// preview and apply. On non-Unix platforms creation time is the available
// std identity; this is not a descriptor-bound filesystem transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DirectoryIdentity {
    path: PathBuf,
    created: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

fn directories(
    checkout: &Path,
    relative: &Path,
) -> Result<Vec<DirectoryIdentity>, ObservationFailure> {
    use ObservationFailure::Changed;
    let mut path = checkout.to_path_buf();
    let mut result = vec![];
    let mut components = relative.components();
    loop {
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Changed(format!("A skill ancestor is absent: {}", path.display()))
            } else {
                ObservationFailure::io(error.to_string(), error)
            }
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(Changed(format!(
                "A skill ancestor is not a physical directory: {}",
                path.display()
            )));
        }
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        result.push(DirectoryIdentity {
            path: path.clone(),
            created: metadata.created().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        });
        match components.next() {
            None => break,
            Some(Component::Normal(name)) => path.push(name),
            Some(Component::CurDir) if relative == Path::new(".") => break,
            _ => {
                return Err(Changed(
                    "Skill path must stay inside its registered source".into(),
                ));
            }
        }
    }
    Ok(result)
}

pub(crate) fn begin(
    source: &RegisteredSource,
    variant: VariantRef,
    store: &Store,
) -> Result<AdoptionDraft, AdoptionFailure> {
    let checkout = source.git_top_level().to_path_buf();
    let identity = source
        .repository_identity()
        .cloned()
        .ok_or("Source identity is unproven; re-register the source first")?;
    if repository_identity(&checkout).map_err(|e| e.to_string())? != identity {
        return Err("The registered repository identity changed".into());
    }
    directories(&checkout, variant.variant_relative_path())?;
    if store
        .origin_record(
            variant.source_id(),
            variant.catalog_relative_path(),
            variant.variant_relative_path(),
        )
        .map_err(AdoptionFailure::metadata)?
        .is_some()
    {
        return Err("This variant already has a baseline; adoption cannot replace it".into());
    }
    let evidence = read_evidence(
        &checkout,
        &checkout.join(variant.variant_relative_path()),
        variant.skill_name(),
    );
    let fields = evidence.suggested_fields();
    Ok(AdoptionDraft {
        fields,
        focused: 0,
        evidence,
        error: None,
        variant,
        checkout,
        identity,
    })
}

pub(crate) fn plan(draft: &AdoptionDraft, store: &Store) -> Result<AdoptionPlan, AdoptionFailure> {
    let origin = Origin::from_input(&draft.fields[0], &draft.fields[1])?;
    let update_ref = draft.fields[2].trim().to_owned();
    crate::provenance::validate_update_ref(&update_ref)?;
    if !update_ref.starts_with("refs/heads/") {
        return Err("Enter an explicit tracking branch such as refs/heads/main".into());
    }
    let observation = observe(&draft.checkout, &draft.variant, &draft.identity, None)?;
    if observation.evidence != draft.evidence {
        return Err("Origin evidence changed; close and reopen adoption".into());
    }
    let origin = observation.evidence.resolve(origin)?;
    let result = AdoptionPlan {
        record: OriginRecord {
            source_id: draft.variant.source_id(),
            catalog_relative_path: draft.variant.catalog_relative_path().into(),
            variant_relative_path: draft.variant.variant_relative_path().into(),
            origin,
            update_ref,
            baseline: observation.baseline.clone(),
        },
        variant: draft.variant.clone(),
        checkout: draft.checkout.clone(),
        database: store.database_path().into(),
        observation,
        identity: draft.identity.clone(),
    };
    recheck(&result)?;
    let guard = store
        .origin_record(
            result.record.source_id,
            &result.record.catalog_relative_path,
            &result.record.variant_relative_path,
        )
        .map_err(AdoptionFailure::metadata)?;
    if guard.is_some() {
        return Err("This variant already has a baseline".into());
    }
    Ok(result)
}

/// One observation recipe for preview capture and every subsequent guard.
/// Expected identities are checked before reading skill content.
/// Repeated captures detect observed changes; they do not lock external writers.
#[derive(Clone, Debug, Eq, PartialEq)]
struct AdoptionObservation {
    directories: Vec<DirectoryIdentity>,
    evidence: Evidence,
    baseline: Baseline,
}

impl From<ObservationFailure> for AdoptionFailure {
    fn from(failure: ObservationFailure) -> Self {
        match failure {
            ObservationFailure::Changed(message) | ObservationFailure::Unavailable(message) => {
                message.into()
            }
        }
    }
}

fn observe(
    checkout: &Path,
    variant: &VariantRef,
    identity: &RepositoryIdentity,
    expected: Option<&AdoptionObservation>,
) -> Result<AdoptionObservation, ObservationFailure> {
    use ObservationFailure::{Changed, Unavailable};
    if repository_identity(checkout).map_err(|e| Unavailable(e.to_string()))? != *identity {
        return Err(Changed("Paths changed after the adoption preview".into()));
    }
    let directories = directories(checkout, variant.variant_relative_path())?;
    if expected.is_some_and(|before| before.directories != directories) {
        return Err(Changed("Paths changed after the adoption preview".into()));
    }
    let skill = checkout.join(variant.variant_relative_path());
    let evidence = read_evidence(checkout, &skill, variant.skill_name());
    if let Some(before) = expected
        && before.evidence != evidence
    {
        // Unreadable evidence is not proof that its bytes changed. Preserve
        // the guard's fail-fast order: Incomplete means later content checks
        // were withheld, not that those unchecked postconditions agree.
        return Err(if !evidence.change_is_unavailable(&before.evidence) {
            Changed("Origin evidence changed after the adoption preview".into())
        } else {
            Unavailable(format!(
                "Origin evidence changed or could not be read after the adoption preview: {}",
                evidence.problems.join("; ")
            ))
        });
    }
    let validated = crate::validation::validate_portable_skill(&skill)
        .map_err(|error| validation_failure(&skill, error))?;
    if validated.name() != variant.skill_name() {
        return Err(Changed("The selected skill identity changed".into()));
    }
    let baseline = observe_directory_hash(&skill)?;
    if expected.is_some_and(|before| before.baseline != baseline) {
        return Err(Changed(
            "Skill content changed after the adoption preview".into(),
        ));
    }
    Ok(AdoptionObservation {
        directories,
        evidence,
        baseline,
    })
}

fn validation_failure(
    skill: &Path,
    error: crate::validation::PortableValidationError,
) -> ObservationFailure {
    use crate::validation::PortableValidationError::*;
    use ObservationFailure::{Changed, Unavailable};
    let message = error.to_string();
    match error {
        // MissingSkillMd can also hide a directory-entry file_type error.
        // Establish absence or a non-file explicitly before calling it a change.
        MissingSkillMd => match fs::symlink_metadata(skill.join("SKILL.md")) {
            Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
                Changed(message)
            }
            Ok(_) => Unavailable(message),
            Err(error) => ObservationFailure::io(message, error),
        },
        UnreadableSkillMd(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            Changed(message)
        }
        UnreadableSkillMd(error) | ReadDirectory { source: error, .. } => {
            ObservationFailure::io(message, error)
        }
        SourceInspectionLimitExceeded => Unavailable(message),
        // A successfully observed size or entry-count violation also disagrees
        // with the valid skill captured in the preview.
        _ => Changed(message),
    }
}

fn recheck(plan: &AdoptionPlan) -> Result<(), ObservationFailure> {
    observe(
        &plan.checkout,
        &plan.variant,
        &plan.identity,
        Some(&plan.observation),
    )
    .map(|_| ())
}

/// `Err` means nothing was saved. Once commit succeeds, all exits return `Ok`
/// with a typed verification outcome, including unavailable postconditions.
pub(crate) fn apply(
    plan: &AdoptionPlan,
    store: &mut Store,
) -> Result<AdoptionVerification, AdoptionFailure> {
    let transaction = store.begin_mutation().map_err(AdoptionFailure::metadata)?;
    if !transaction
        .variant_registration_matches(&plan.variant, &plan.checkout)
        .map_err(AdoptionFailure::metadata)?
    {
        return Err("The selected variant registration changed after preview".into());
    }
    recheck(plan)?;
    transaction
        .record_origin(&plan.record)
        .map_err(AdoptionFailure::metadata)?;
    // Retain the second reading before commit: a change during insertion rolls back.
    recheck(plan)?;
    transaction.commit().map_err(AdoptionFailure::metadata)?;
    Ok(verify_saved(plan, store))
}

fn verify_saved(plan: &AdoptionPlan, store: &Store) -> AdoptionVerification {
    match store.origin_record(
        plan.record.source_id,
        &plan.record.catalog_relative_path,
        &plan.record.variant_relative_path,
    ) {
        Err(error) => return AdoptionVerification::Incomplete(AdoptionFailure::metadata(error)),
        Ok(record) if record != Some(plan.record.clone()) => {
            return AdoptionVerification::Failed(AdoptionFailure {
                metadata: MetadataAvailability::Unavailable,
                message: "stored metadata differs from the confirmed baseline".into(),
            });
        }
        Ok(_) => {}
    }
    match recheck(plan) {
        Ok(()) => AdoptionVerification::Verified,
        Err(ObservationFailure::Changed(message)) => AdoptionVerification::Failed(message.into()),
        Err(ObservationFailure::Unavailable(message)) => {
            AdoptionVerification::Incomplete(message.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppEnvironment, SkilledApp};
    use std::process::Command;

    fn fixture() -> (tempfile::TempDir, Store, AdoptionPlan) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("source");
        fs::create_dir_all(root.join("skills/demo")).unwrap();
        fs::write(
            root.join("skills/demo/SKILL.md"),
            "---\nname: demo\ndescription: Fixture\n---\nBody\n",
        )
        .unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
                "commit",
                "-m",
                "initial",
            ],
        ] {
            let output = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let mut app = SkilledApp::open(AppEnvironment::new(
            temp.path().join("home"),
            temp.path().join("data"),
            "",
        ))
        .unwrap();
        app.confirm_source(app.preview_source(&root).unwrap())
            .unwrap();
        let source = &app.sources()[0];
        let catalog = source
            .catalogs()
            .iter()
            .find(|c| !c.candidates().is_empty())
            .unwrap();
        let variant = VariantRef::of(source, catalog, &catalog.candidates()[0]);
        let store = Store::open(&temp.path().join("data")).unwrap();
        let mut draft = begin(source, variant, &store).unwrap();
        draft.fields = [
            "https://github.com/example/upstream".into(),
            "skills/demo".into(),
            "refs/heads/main".into(),
        ];
        let plan = plan(&draft, &store).unwrap();
        (temp, store, plan)
    }

    #[test]
    fn validation_path_type_errors_are_changes_and_io_failures_remain_unavailable() {
        use crate::validation::PortableValidationError;
        use std::io::{Error, ErrorKind};
        let temp = tempfile::tempdir().unwrap();
        let skill = temp.path().join("skill");
        for kind in [
            ErrorKind::NotADirectory,
            ErrorKind::IsADirectory,
            ErrorKind::NotFound,
            ErrorKind::PermissionDenied,
            ErrorKind::Interrupted,
        ] {
            let should_change = matches!(
                kind,
                ErrorKind::NotADirectory | ErrorKind::IsADirectory | ErrorKind::NotFound
            );
            // Exercise both errors the validator can return in the window
            // after ancestor inspection, without racing a background writer.
            for directory_read in [false, true] {
                let error = if directory_read {
                    PortableValidationError::ReadDirectory {
                        path: skill.clone(),
                        source: Error::from(kind),
                    }
                } else {
                    PortableValidationError::UnreadableSkillMd(Error::from(kind))
                };
                let failure = validation_failure(&skill, error);
                assert_eq!(
                    matches!(failure, ObservationFailure::Changed(_)),
                    should_change,
                    "{kind:?}: {failure:?}"
                );
            }
        }
    }

    #[test]
    fn missing_document_recheck_recognizes_a_skill_replaced_by_a_file() {
        let temp = tempfile::tempdir().unwrap();
        let skill = temp.path().join("skill");
        fs::write(&skill, "replacement file").unwrap();
        let failure = validation_failure(
            &skill,
            crate::validation::PortableValidationError::MissingSkillMd,
        );
        assert!(
            matches!(failure, ObservationFailure::Changed(_)),
            "{failure:?}"
        );
    }

    #[test]
    fn saved_content_disagreement_and_unavailable_reads_have_distinct_outcomes() {
        let (temp, mut store, plan) = fixture();
        assert_eq!(
            apply(&plan, &mut store).unwrap(),
            AdoptionVerification::Verified
        );
        let skill = temp.path().join("source/skills/demo");
        fs::write(skill.join("untracked"), "changed").unwrap();
        assert!(
            matches!(verify_saved(&plan, &store), AdoptionVerification::Failed(failure)
            if failure.metadata == MetadataAvailability::Available)
        );
        fs::remove_file(skill.join("untracked")).unwrap();
        #[cfg(unix)]
        if unsafe { libc::geteuid() } != 0 {
            use std::os::unix::fs::PermissionsExt;
            let evidence = temp.path().join("source/ATTRIBUTION.md");
            fs::write(&evidence, "unreadable").unwrap();
            fs::set_permissions(&evidence, fs::Permissions::from_mode(0o000)).unwrap();
            assert!(
                matches!(verify_saved(&plan, &store), AdoptionVerification::Incomplete(failure)
                if failure.metadata == MetadataAvailability::Available)
            );
            // A second, successfully read change still proves disagreement.
            let lock = temp.path().join("source/skills-lock.json");
            fs::write(&lock, "invalid JSON").unwrap();
            assert!(matches!(
                verify_saved(&plan, &store),
                AdoptionVerification::Failed(_)
            ));
            fs::remove_file(lock).unwrap();
            fs::remove_file(evidence).unwrap();
            let unreadable = skill.join("unreadable");
            fs::write(&unreadable, "content").unwrap();
            fs::set_permissions(unreadable, fs::Permissions::from_mode(0o000)).unwrap();
            assert!(matches!(
                verify_saved(&plan, &store),
                AdoptionVerification::Incomplete(_)
            ));
        }
        assert_eq!(
            store
                .origin_record(
                    plan.record.source_id,
                    &plan.record.catalog_relative_path,
                    &plan.record.variant_relative_path
                )
                .unwrap(),
            Some(plan.record)
        );
    }

    #[test]
    fn observed_missing_invalid_and_oversized_documents_fail_saved_verification() {
        for content in [
            None,
            Some(b"invalid document".to_vec()),
            Some(vec![0xff]),
            Some(vec![b'x'; 1024 * 1024 + 1]),
        ] {
            let (temp, mut store, plan) = fixture();
            assert_eq!(
                apply(&plan, &mut store).unwrap(),
                AdoptionVerification::Verified
            );
            let path = temp.path().join("source/skills/demo/SKILL.md");
            match content {
                None => fs::remove_file(path).unwrap(),
                Some(bytes) => fs::write(path, bytes).unwrap(),
            }
            let verification = verify_saved(&plan, &store);
            assert!(
                matches!(verification, AdoptionVerification::Failed(_)),
                "{verification:?}"
            );
        }
    }

    #[test]
    fn observed_invalid_evidence_fails_saved_verification() {
        for (name, bytes) in [
            (
                "ATTRIBUTION.md",
                b"| Skill | Source |\n| `demo` | invalid |\n".to_vec(),
            ),
            ("skills-lock.json", b"invalid JSON".to_vec()),
            ("ATTRIBUTION.md", vec![0xff]),
            ("ATTRIBUTION.md", vec![b'x'; 1024 * 1024 + 1]),
        ] {
            let (temp, mut store, plan) = fixture();
            assert_eq!(
                apply(&plan, &mut store).unwrap(),
                AdoptionVerification::Verified
            );
            fs::write(temp.path().join("source").join(name), bytes).unwrap();
            let verification = verify_saved(&plan, &store);
            assert!(
                matches!(verification, AdoptionVerification::Failed(_)),
                "{verification:?}"
            );
        }
    }

    #[test]
    fn observed_baseline_limits_fail_saved_verification() {
        for excessive_depth in [false, true] {
            let (temp, mut store, plan) = fixture();
            assert_eq!(
                apply(&plan, &mut store).unwrap(),
                AdoptionVerification::Verified
            );
            let skill = temp.path().join("source/skills/demo");
            if excessive_depth {
                let path = (0..33).fold(skill, |path, _| path.join("d"));
                fs::create_dir_all(path).unwrap();
            } else {
                fs::File::create(skill.join("oversized"))
                    .unwrap()
                    .set_len(32 * 1024 * 1024 + 1)
                    .unwrap();
            }
            let verification = verify_saved(&plan, &store);
            assert!(
                matches!(verification, AdoptionVerification::Failed(_)),
                "{verification:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_observed_unsupported_entry_fails_saved_verification() {
        let (temp, mut store, plan) = fixture();
        assert_eq!(
            apply(&plan, &mut store).unwrap(),
            AdoptionVerification::Verified
        );
        // A socket needs no content read and cannot appear in a valid baseline.
        let socket = temp.path().join("source/skills/demo/socket");
        let _listener = std::os::unix::net::UnixListener::bind(socket).unwrap();
        assert!(matches!(
            verify_saved(&plan, &store),
            AdoptionVerification::Failed(_)
        ));
    }

    #[test]
    fn same_content_directory_replacement_is_refused_by_the_shared_observation() {
        let (temp, mut store, plan) = fixture();
        let skill = temp.path().join("source/skills/demo");
        let bytes = fs::read(skill.join("SKILL.md")).unwrap();
        fs::rename(&skill, temp.path().join("moved")).unwrap();
        fs::create_dir(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), bytes).unwrap();
        assert!(matches!(
            recheck(&plan),
            Err(ObservationFailure::Changed(_))
        ));
        assert!(apply(&plan, &mut store).is_err());
        assert_eq!(
            store
                .origin_record(
                    plan.record.source_id,
                    &plan.record.catalog_relative_path,
                    &plan.record.variant_relative_path
                )
                .unwrap(),
            None
        );
    }
}
