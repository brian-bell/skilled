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
    provenance::{Baseline, Evidence, Origin, directory_hash, read_evidence},
    resolution::VariantRef,
    source::{RegisteredSource, RepositoryIdentity, repository_identity},
    store::Store,
};

/// A refusal of the request is separate from unavailable private metadata.
#[derive(Debug)]
pub(crate) struct AdoptionFailure {
    pub message: String,
    pub metadata: bool,
}
impl From<String> for AdoptionFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            metadata: false,
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
            metadata: !matches!(error, crate::Error::SourceChangedAfterPreview),
            message: error.to_string(),
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
    pub(crate) evidence: Evidence,
    pub(crate) identity: RepositoryIdentity,
    pub(crate) directories: Vec<DirectoryIdentity>,
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
    Report(String),
    Failed(String),
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

fn directories(checkout: &Path, relative: &Path) -> Result<Vec<DirectoryIdentity>, String> {
    let mut path = checkout.to_path_buf();
    let mut result = vec![];
    let mut components = relative.components();
    loop {
        let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(format!(
                "A skill ancestor is not a physical directory: {}",
                path.display()
            ));
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
            _ => return Err("Skill path must stay inside its registered source".into()),
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
    let skill = draft.checkout.join(draft.variant.variant_relative_path());
    let evidence = read_evidence(&draft.checkout, &skill, draft.variant.skill_name());
    if evidence != draft.evidence {
        return Err("Origin evidence changed; close and reopen adoption".into());
    }
    let origin = evidence.resolve(origin)?;
    let identities = directories(&draft.checkout, draft.variant.variant_relative_path())?;
    let validated =
        crate::validation::validate_portable_skill(&skill).map_err(|e| e.to_string())?;
    if validated.name() != draft.variant.skill_name() {
        return Err("The selected skill identity changed".into());
    }
    let baseline = directory_hash(&skill)?;
    let result = AdoptionPlan {
        record: OriginRecord {
            source_id: draft.variant.source_id(),
            catalog_relative_path: draft.variant.catalog_relative_path().into(),
            variant_relative_path: draft.variant.variant_relative_path().into(),
            origin,
            update_ref,
            baseline,
        },
        variant: draft.variant.clone(),
        checkout: draft.checkout.clone(),
        database: store.database_path().into(),
        evidence,
        identity: draft.identity.clone(),
        directories: identities,
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

fn recheck(plan: &AdoptionPlan) -> Result<(), String> {
    if repository_identity(&plan.checkout).map_err(|e| e.to_string())? != plan.identity
        || directories(&plan.checkout, &plan.record.variant_relative_path)? != plan.directories
    {
        return Err("Paths changed after the adoption preview".into());
    }
    let skill = plan.checkout.join(&plan.record.variant_relative_path);
    if read_evidence(&plan.checkout, &skill, plan.variant.skill_name()) != plan.evidence {
        return Err("Origin evidence changed after the adoption preview".into());
    }
    let validated =
        crate::validation::validate_portable_skill(&skill).map_err(|e| e.to_string())?;
    if validated.name() != plan.variant.skill_name() {
        return Err("The selected skill identity changed".into());
    }
    if directory_hash(&skill)? != plan.record.baseline {
        return Err("Skill content changed after the adoption preview".into());
    }
    Ok(())
}

pub(crate) fn apply(plan: &AdoptionPlan, store: &mut Store) -> Result<(), AdoptionFailure> {
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
    // A second reading before commit rolls the metadata back if a concurrent
    // writer changed the content while SQLite prepared the row.
    recheck(plan)?;
    transaction.commit().map_err(AdoptionFailure::metadata)?;
    if store
        .origin_record(
            plan.record.source_id,
            &plan.record.catalog_relative_path,
            &plan.record.variant_relative_path,
        )
        .map_err(|e| AdoptionFailure {
            metadata: true,
            message: format!("Baseline saved; verification incomplete: {e}"),
        })?
        != Some(plan.record.clone())
    {
        return Err(AdoptionFailure {
            metadata: true,
            message:
                "Baseline saved; metadata verification failed because the stored record differs"
                    .into(),
        });
    }
    recheck(plan)
        .map_err(|e| format!("Baseline saved, but post-save verification failed: {e}").into())
}
