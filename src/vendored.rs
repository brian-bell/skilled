//! Read-only checks and immutable previews for an explicitly adopted skill.
//!
//! This module deliberately does not replace files or write provenance.  It
//! captures the local tree before contacting the origin, refuses drift, and
//! builds the complete expected tree from a pinned origin snapshot.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Child,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::{
    adoption::{self, AdoptionFailure, MetadataAvailability, OriginRecord},
    git::origin::{self, OriginEntry},
    inventory::{InstalledSkillObservation, InventorySnapshot},
    provenance::{
        Baseline, DirectoryManifest, ManifestEntryKind,
        baseline_from_regular_entries_with_directory_modes,
    },
    resolution::VariantRef,
    source::{RegisteredSource, RepositoryIdentity},
    store::Store,
};

/// Everything a worker needs after the UI has explicitly requested a check.
/// The local manifest was captured before any network work and is rechecked on
/// both sides of the fetch.
#[derive(Clone, Debug)]
pub(crate) struct CheckRequest {
    source: RegisteredSource,
    variant: VariantRef,
    record: OriginRecord,
    checkout: PathBuf,
    identity: RepositoryIdentity,
    directories: Vec<adoption::DirectoryIdentity>,
    local: DirectoryManifest,
    affected_installations: Vec<AffectedInstallation>,
    environment: crate::AppEnvironment,
}

/// An installation that resolves to the checked variant.  Aliases are kept as
/// separate paths because replacement verification has to account for each
/// link which exposes the selected directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AffectedInstallation {
    path: PathBuf,
    agent: crate::AgentKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Preview {
    revision: String,
    origin_repository: String,
    origin_subdirectory: String,
    update_ref: String,
    checkout: PathBuf,
    skill: PathBuf,
    old_baseline: Baseline,
    proposed_baseline: Baseline,
    changes: Vec<FileChange>,
    expected_entries: Vec<ExpectedEntry>,
    affected_installations: Vec<AffectedInstallation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExpectedEntry {
    relative_path: PathBuf,
    kind: ManifestEntryKind,
    executable: bool,
    bytes: Vec<u8>,
}

#[derive(Debug)]
struct PlannedTree {
    manifest: DirectoryManifest,
    preserved_notices: Vec<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileChange {
    destination: PathBuf,
    kind: FileChangeKind,
    before_bytes: usize,
    after_bytes: usize,
    before_lines: Option<usize>,
    after_lines: Option<usize>,
    before_executable: bool,
    after_executable: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FileChangeKind {
    Add,
    Remove,
    Modify,
    PreserveNotice,
}

impl Preview {
    pub fn is_noop(&self) -> bool {
        self.old_baseline == self.proposed_baseline
    }

    /// All filesystem targets are shown as absolute paths. This is a
    /// read-only preview; applying it belongs to the later executor slice.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            "Check complete; this preview does not write files or metadata.".into(),
            format!("Source: {}", self.checkout.display()),
            format!("Skill: {}", self.skill.display()),
            format!("Origin: {}", self.origin_repository),
            format!("Origin subdirectory: {}", self.origin_subdirectory),
            format!("Tracking branch: {}", self.update_ref),
            format!("Pinned origin revision: {}", self.revision),
            format!(
                "Adopted baseline v{}: {}",
                self.old_baseline.version, self.old_baseline.digest
            ),
            format!(
                "Proposed baseline v{}: {}",
                self.proposed_baseline.version, self.proposed_baseline.digest
            ),
            format!(
                "Expected regular files: {}",
                self.expected_entries
                    .iter()
                    .filter(|entry| entry.kind == ManifestEntryKind::File)
                    .count()
            ),
        ];
        if self.changes.is_empty() {
            lines.push("No content changes are available.".into());
        } else {
            lines.push("Planned file changes:".into());
            for change in &self.changes {
                let detail = match change.kind {
                    FileChangeKind::Add => format!(
                        "add {} bytes{}{}",
                        change.after_bytes,
                        line_suffix(change.after_lines),
                        mode_suffix(false, change.after_executable)
                    ),
                    FileChangeKind::Remove => format!(
                        "remove {} bytes{}{}",
                        change.before_bytes,
                        line_suffix(change.before_lines),
                        mode_suffix(change.before_executable, false)
                    ),
                    FileChangeKind::Modify => format!(
                        "modify {} → {} bytes{}{}",
                        change.before_bytes,
                        change.after_bytes,
                        changed_line_suffix(change.before_lines, change.after_lines),
                        mode_suffix(change.before_executable, change.after_executable)
                    ),
                    FileChangeKind::PreserveNotice => format!(
                        "preserve existing notice ({} bytes{})",
                        change.before_bytes,
                        line_suffix(change.before_lines)
                    ),
                };
                lines.push(format!("{}: {detail}", change.destination.display()));
            }
        }
        if !self.affected_installations.is_empty() {
            lines.push("Affected installations:".into());
            for installation in &self.affected_installations {
                lines.push(format!(
                    "{}: {}",
                    installation.agent.display_name(),
                    installation.path.display()
                ));
            }
        }
        lines
    }

    #[cfg(test)]
    pub(crate) fn fixture() -> Self {
        let old_baseline = Baseline {
            version: 1,
            digest: "1".repeat(64),
        };
        let proposed_baseline = Baseline {
            version: 1,
            digest: "2".repeat(64),
        };
        Self {
            revision: "0123456789abcdef0123456789abcdef01234567".into(),
            origin_repository: "https://github.com/example/demo".into(),
            origin_subdirectory: "skills/demo".into(),
            update_ref: "refs/heads/main".into(),
            checkout: PathBuf::from("/source"),
            skill: PathBuf::from("/source/skills/demo"),
            old_baseline,
            proposed_baseline,
            changes: vec![FileChange {
                destination: PathBuf::from("/source/skills/demo/SKILL.md"),
                kind: FileChangeKind::Modify,
                before_bytes: 12,
                after_bytes: 18,
                before_lines: Some(3),
                after_lines: Some(4),
                before_executable: false,
                after_executable: false,
            }],
            expected_entries: vec![ExpectedEntry {
                relative_path: PathBuf::from("SKILL.md"),
                kind: ManifestEntryKind::File,
                executable: false,
                bytes: b"---\nname: demo\ndescription: new\n---\n".to_vec(),
            }],
            affected_installations: vec![AffectedInstallation {
                path: PathBuf::from("/installed/demo"),
                agent: crate::AgentKind::Codex,
            }],
        }
    }
}

fn line_suffix(lines: Option<usize>) -> String {
    lines
        .map(|count| format!(", {count} lines"))
        .unwrap_or_else(|| ", binary".into())
}
fn changed_line_suffix(before: Option<usize>, after: Option<usize>) -> String {
    match (before, after) {
        (Some(before), Some(after)) => format!(", {before} → {after} lines"),
        _ => ", binary".into(),
    }
}
fn mode_suffix(before: bool, after: bool) -> String {
    if before != after {
        format!(", executable {before} → {after}")
    } else {
        String::new()
    }
}

/// Refuse an unproven or locally modified variant before network activity.
pub(crate) fn prepare(
    source: &RegisteredSource,
    variant: VariantRef,
    store: &Store,
    inventory: &InventorySnapshot,
    environment: &crate::AppEnvironment,
) -> Result<CheckRequest, AdoptionFailure> {
    let checkout = source.git_top_level().to_path_buf();
    let identity = source
        .repository_identity()
        .cloned()
        .ok_or("Source identity is unproven; re-register the source first")?;
    let record = store
        .origin_record(
            variant.source_id(),
            variant.catalog_relative_path(),
            variant.variant_relative_path(),
        )
        .map_err(metadata_failure)?
        .ok_or("This variant has no adopted origin baseline")?;
    let local = adoption::observe_variant_manifest(&checkout, &variant, &identity)
        .map_err(observation_failure)?;
    validate_local_manifest(&local)?;
    if local.baseline != record.baseline {
        return Err(
            "Vendored skill content differs from its adopted baseline; update is blocked".into(),
        );
    }
    if !inventory.counts_are_complete() || !inventory.registry_is_complete() {
        return Err(
            "Installation scan is incomplete; affected installations cannot be verified".into(),
        );
    }
    let directories = adoption::directories(&checkout, variant.variant_relative_path())
        .map_err(observation_failure)?;
    let mut affected_installations = affected_installations(inventory, &variant);
    affected_installations.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(CheckRequest {
        source: source.clone(),
        variant,
        record,
        checkout,
        identity,
        directories,
        local,
        affected_installations,
        environment: environment.clone(),
    })
}

/// Fetch the confirmed origin, pin its revision, and build the full immutable
/// output tree. `Ok(None)` means cancellation, not a partial preview.
pub(crate) fn check(
    request: CheckRequest,
    data_dir: &Path,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<Option<Preview>, AdoptionFailure> {
    recheck_request(&request, data_dir)?;
    if cancelled.load(Ordering::Acquire) {
        return Ok(None);
    }
    let Some(snapshot) = origin::fetch_snapshot(
        &data_dir.join("vendored-origin-cache"),
        &request.record.origin,
        &request.record.update_ref,
        cancelled,
        child_slot,
    )
    .map_err(AdoptionFailure::from)?
    else {
        return Ok(None);
    };
    recheck_request(&request, data_dir)?;
    recheck_affected_installations(&request, data_dir)?;
    let final_tree = final_manifest(
        &request.local,
        &snapshot.entries,
        &snapshot.notices,
        request.variant.skill_name(),
    )?;
    let changes = changes(
        &request.local,
        &final_tree.manifest,
        &request.checkout,
        request.variant.variant_relative_path(),
    )
    .into_iter()
    .chain(preserved_notice_changes(
        &request.local,
        &final_tree.preserved_notices,
        &request.checkout,
        request.variant.variant_relative_path(),
    ))
    .collect();
    let expected_entries = final_tree
        .manifest
        .entries
        .iter()
        .map(|entry| ExpectedEntry {
            relative_path: entry.relative_path.clone(),
            kind: entry.kind,
            executable: entry.executable,
            bytes: entry.bytes.clone(),
        })
        .collect();
    Ok(Some(Preview {
        revision: snapshot.revision,
        origin_repository: request.record.origin.repository().to_owned(),
        origin_subdirectory: request.record.origin.subdirectory().to_owned(),
        update_ref: request.record.update_ref.clone(),
        checkout: request.checkout.clone(),
        skill: request
            .checkout
            .join(request.variant.variant_relative_path()),
        old_baseline: request.record.baseline,
        proposed_baseline: final_tree.manifest.baseline,
        changes,
        expected_entries,
        affected_installations: request.affected_installations,
    }))
}

fn recheck_affected_installations(
    request: &CheckRequest,
    data_dir: &Path,
) -> Result<(), AdoptionFailure> {
    let store = Store::open(data_dir).map_err(metadata_failure)?;
    let sources = store.registered_sources().map_err(metadata_failure)?;
    let agents = crate::agents::detect_agents(&request.environment);
    let refreshed = crate::inventory::scan_installations(
        &agents,
        &sources,
        crate::inventory::RegistryAvailability::Readable,
    );
    if !refreshed.counts_are_complete() || !refreshed.registry_is_complete() {
        return Err("Installation scan became incomplete during the origin check".into());
    }
    let mut refreshed_affected = affected_installations(&refreshed, &request.variant);
    refreshed_affected.sort_by(|left, right| left.path.cmp(&right.path));
    if refreshed_affected != request.affected_installations {
        return Err("Affected installations changed during the origin check".into());
    }
    Ok(())
}

fn recheck_request(request: &CheckRequest, data_dir: &Path) -> Result<(), AdoptionFailure> {
    let store = Store::open(data_dir).map_err(metadata_failure)?;
    let current = store
        .origin_record(
            request.variant.source_id(),
            request.variant.catalog_relative_path(),
            request.variant.variant_relative_path(),
        )
        .map_err(metadata_failure)?;
    if current != Some(request.record.clone()) {
        return Err("The adopted origin record changed after the update check began".into());
    }
    let sources = store.registered_sources().map_err(metadata_failure)?;
    if !sources
        .iter()
        .any(|source| registration_matches(source, &request.source, &request.variant))
    {
        return Err("The selected source registration changed after the update check began".into());
    }
    let directories =
        adoption::directories(&request.checkout, request.variant.variant_relative_path())
            .map_err(observation_failure)?;
    if directories != request.directories {
        return Err("Selected skill paths changed after the update check began".into());
    }
    let local =
        adoption::observe_variant_manifest(&request.checkout, &request.variant, &request.identity)
            .map_err(observation_failure)?;
    validate_local_manifest(&local)?;
    if local != request.local {
        return Err("Vendored skill content changed after the update check began".into());
    }
    Ok(())
}

fn registration_matches(
    current: &RegisteredSource,
    expected: &RegisteredSource,
    variant: &VariantRef,
) -> bool {
    current.id() == expected.id()
        && current.label() == expected.label()
        && current.git_top_level() == expected.git_top_level()
        && current.repository_identity() == expected.repository_identity()
        && current.catalogs().iter().any(|catalog| {
            catalog.included()
                && catalog.relative_path() == variant.catalog_relative_path()
                && catalog.classification() == variant.classification()
                && catalog.compatibility() == variant.compatibility()
        })
}

fn affected_installations(
    inventory: &InventorySnapshot,
    variant: &VariantRef,
) -> Vec<AffectedInstallation> {
    inventory
        .rows()
        .iter()
        .flat_map(|row| row.observations())
        .filter(|observation| observation.resolution() == Some(variant))
        .map(
            |observation: &InstalledSkillObservation| AffectedInstallation {
                path: observation.path().to_path_buf(),
                agent: observation.agent(),
            },
        )
        .collect()
}

fn final_manifest(
    local: &DirectoryManifest,
    candidate: &[OriginEntry],
    upstream_notices: &[OriginEntry],
    skill_name: &str,
) -> Result<PlannedTree, AdoptionFailure> {
    let mut final_files = BTreeMap::<PathBuf, (bool, Vec<u8>)>::new();
    for entry in candidate {
        safe_relative(&entry.path)?;
        if final_files
            .insert(
                entry.path.clone(),
                (normalized_executable(entry.executable), entry.bytes.clone()),
            )
            .is_some()
        {
            return Err(format!(
                "Origin contains duplicate candidate path: {}",
                entry.path.display()
            )
            .into());
        }
    }
    for notice in upstream_notices {
        safe_relative(&notice.path)?;
        if !is_notice(&notice.path) {
            return Err(format!(
                "Origin supplied a non-notice ancestor file: {}",
                notice.path.display()
            )
            .into());
        }
        match final_files.get(&notice.path) {
            None => {
                final_files.insert(
                    notice.path.clone(),
                    (
                        normalized_executable(notice.executable),
                        notice.bytes.clone(),
                    ),
                );
            }
            Some((executable, bytes))
                if *executable == normalized_executable(notice.executable)
                    && *bytes == notice.bytes => {}
            Some(_) => {
                return Err(format!(
                    "Origin ancestor notice conflicts with selected skill content: {}",
                    notice.path.display()
                )
                .into());
            }
        }
    }
    validate_candidate_tree(&final_files)?;
    let skill_md = final_files
        .get(Path::new("SKILL.md"))
        .ok_or("Origin candidate does not contain SKILL.md")?;
    crate::validation::validate_skill_document_bytes(skill_name, &skill_md.1)
        .map_err(|error| format!("Origin candidate is not a portable skill: {error}"))?;

    let mut preserved_notices = Vec::new();
    for entry in &local.entries {
        match entry.kind {
            ManifestEntryKind::Directory => {}
            ManifestEntryKind::Symlink => {
                return Err(format!(
                    "Selected skill contains a symlink that cannot be safely replaced: {}",
                    entry.relative_path.display()
                )
                .into());
            }
            ManifestEntryKind::File if is_notice(&entry.relative_path) => {
                match final_files.get(&entry.relative_path) {
                    None => {
                        preserved_notices.push(entry.relative_path.clone());
                        final_files.insert(
                            entry.relative_path.clone(),
                            (entry.executable, entry.bytes.clone()),
                        );
                    }
                    Some((executable, bytes))
                        if *executable == entry.executable && *bytes == entry.bytes => {}
                    Some(_) => {
                        return Err(format!(
                            "Origin conflicts with existing notice that must be preserved: {}",
                            entry.relative_path.display()
                        )
                        .into());
                    }
                }
            }
            ManifestEntryKind::File => {}
        }
    }
    validate_candidate_tree(&final_files)?;
    let files = final_files
        .into_iter()
        .map(|(path, (executable, bytes))| (path, executable, bytes))
        .collect::<Vec<_>>();
    let executable_directories = local
        .entries
        .iter()
        .filter(|entry| entry.kind == ManifestEntryKind::Directory)
        .map(|entry| (entry.relative_path.clone(), entry.executable))
        .collect::<BTreeMap<_, _>>();
    Ok(PlannedTree {
        manifest: baseline_from_regular_entries_with_directory_modes(
            &files,
            &executable_directories,
        )
        .map_err(AdoptionFailure::from)?,
        preserved_notices,
    })
}

fn validate_candidate_tree(
    files: &BTreeMap<PathBuf, (bool, Vec<u8>)>,
) -> Result<(), AdoptionFailure> {
    let mut folded = BTreeMap::<String, &Path>::new();
    for path in files.keys() {
        let key = path
            .to_str()
            .ok_or_else(|| AdoptionFailure::from("Origin candidate path is not UTF-8"))?
            .to_lowercase();
        if let Some(other) = folded.insert(key, path)
            && other != path
        {
            return Err(format!(
                "Origin candidate has a case-conflicting path: {} and {}",
                other.display(),
                path.display()
            )
            .into());
        }
        let mut parent = path.parent();
        while let Some(parent_path) = parent {
            if files.contains_key(parent_path) {
                return Err(format!(
                    "Origin candidate conflicts between a file and descendant: {}",
                    path.display()
                )
                .into());
            }
            parent = parent_path.parent();
        }
    }
    Ok(())
}

/// A future executor must name every removed or changed destination exactly.
/// Directory hashing deliberately accepts platform-native non-UTF-8 names, but
/// an interactive preview cannot safely promise such a pathname in this
/// release, so refuse it before any origin fetch.
fn validate_local_manifest(manifest: &DirectoryManifest) -> Result<(), AdoptionFailure> {
    let mut folded = BTreeMap::<String, &Path>::new();
    for entry in &manifest.entries {
        if entry.relative_path.as_os_str().is_empty() {
            continue;
        }
        safe_relative(&entry.relative_path).map_err(|_| {
            AdoptionFailure::from("Selected skill has an unsafe path that cannot be previewed")
        })?;
        let text = entry
            .relative_path
            .to_str()
            .ok_or_else(|| AdoptionFailure {
                message: "Selected skill has a non-UTF-8 path that cannot be previewed".into(),
                metadata: MetadataAvailability::Available,
            })?;
        let key = text.to_lowercase();
        if let Some(other) = folded.insert(key, &entry.relative_path)
            && other != entry.relative_path
        {
            return Err(format!(
                "Selected skill has case-conflicting paths that cannot be previewed: {} and {}",
                other.display(),
                entry.relative_path.display()
            )
            .into());
        }
    }
    Ok(())
}

fn changes(
    local: &DirectoryManifest,
    final_manifest: &DirectoryManifest,
    checkout: &Path,
    variant: &Path,
) -> Vec<FileChange> {
    let local = regular_files(local);
    let final_files = regular_files(final_manifest);
    let mut paths = local
        .keys()
        .chain(final_files.keys())
        .cloned()
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .filter_map(|path| {
            let before = local.get(&path);
            let after = final_files.get(&path);
            let kind = match (before, after) {
                (None, Some(_)) => FileChangeKind::Add,
                (Some(_), None) => FileChangeKind::Remove,
                (Some(before), Some(after)) if before == after => return None,
                (Some(_), Some(_)) => FileChangeKind::Modify,
                (None, None) => unreachable!(),
            };
            Some(FileChange {
                destination: checkout.join(variant).join(path),
                kind,
                before_bytes: before.map_or(0, |entry| entry.1.len()),
                after_bytes: after.map_or(0, |entry| entry.1.len()),
                before_lines: before.and_then(|entry| line_count(&entry.1)),
                after_lines: after.and_then(|entry| line_count(&entry.1)),
                before_executable: before.is_some_and(|entry| entry.0),
                after_executable: after.is_some_and(|entry| entry.0),
            })
        })
        .collect()
}

fn preserved_notice_changes(
    local: &DirectoryManifest,
    notices: &[PathBuf],
    checkout: &Path,
    variant: &Path,
) -> Vec<FileChange> {
    let files = regular_files(local);
    notices
        .iter()
        .filter_map(|path| {
            files.get(path).map(|entry| FileChange {
                destination: checkout.join(variant).join(path),
                kind: FileChangeKind::PreserveNotice,
                before_bytes: entry.1.len(),
                after_bytes: entry.1.len(),
                before_lines: line_count(&entry.1),
                after_lines: line_count(&entry.1),
                before_executable: entry.0,
                after_executable: entry.0,
            })
        })
        .collect()
}

fn regular_files(manifest: &DirectoryManifest) -> BTreeMap<PathBuf, (bool, Vec<u8>)> {
    manifest
        .entries
        .iter()
        .filter(|entry| entry.kind == ManifestEntryKind::File)
        .map(|entry| {
            (
                entry.relative_path.clone(),
                (entry.executable, entry.bytes.clone()),
            )
        })
        .collect()
}

fn safe_relative(path: &Path) -> Result<(), AdoptionFailure> {
    origin::safe_relative_path(path.as_os_str().as_encoded_bytes())
        .map(|_| ())
        .map_err(AdoptionFailure::from)
}

fn is_notice(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let uppercase = name.to_ascii_uppercase();
    ["LICENSE", "LICENCE", "COPYING", "NOTICE", "ATTRIBUTION"]
        .into_iter()
        .any(|prefix| {
            uppercase == prefix
                || uppercase
                    .strip_prefix(prefix)
                    .is_some_and(|suffix| suffix.starts_with('.') || suffix.starts_with('-'))
        })
}

fn line_count(bytes: &[u8]) -> Option<usize> {
    let text = std::str::from_utf8(bytes).ok()?;
    Some(if text.is_empty() {
        0
    } else {
        text.bytes().filter(|byte| *byte == b'\n').count() + 1
    })
}

fn normalized_executable(executable: bool) -> bool {
    cfg!(unix) && executable
}

fn observation_failure(failure: crate::provenance::ObservationFailure) -> AdoptionFailure {
    match failure {
        crate::provenance::ObservationFailure::Changed(message) => message.into(),
        crate::provenance::ObservationFailure::Unavailable(message) => message.into(),
    }
}

fn metadata_failure(error: crate::Error) -> AdoptionFailure {
    AdoptionFailure::metadata(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::{
        baseline_from_regular_entries_with_directory_modes, observe_directory_hash,
        observe_directory_manifest,
    };
    use crate::{AppEnvironment, SkilledApp, adoption, inventory::RegistryAvailability};
    use std::{fs, process::Command};

    fn adopted_fixture() -> (
        tempfile::TempDir,
        Store,
        RegisteredSource,
        VariantRef,
        InventorySnapshot,
    ) {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        fs::create_dir_all(root.join("skills/demo")).unwrap();
        fs::write(
            root.join("skills/demo/SKILL.md"),
            "---\nname: demo\ndescription: Fixture\n---\n",
        )
        .unwrap();
        for arguments in [
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
                .args(arguments)
                .output()
                .unwrap();
            assert!(output.status.success(), "{:?}", output);
        }
        let environment = AppEnvironment::new(
            temporary.path().join("home"),
            temporary.path().join("data"),
            "",
        );
        let mut app = SkilledApp::open(environment).unwrap();
        app.confirm_source(app.preview_source(&root).unwrap())
            .unwrap();
        let source = app.sources()[0].clone();
        let catalog = source
            .catalogs()
            .iter()
            .find(|catalog| !catalog.candidates().is_empty())
            .unwrap();
        let variant = VariantRef::of(&source, catalog, &catalog.candidates()[0]);
        let mut store = Store::open(&temporary.path().join("data")).unwrap();
        let mut draft = adoption::begin(&source, variant.clone(), &store).unwrap();
        draft.repository = "https://github.com/example/upstream".into();
        draft.subdirectory = "skills/demo".into();
        draft.update_ref = "refs/heads/main".into();
        let plan = adoption::plan(&draft, &store).unwrap();
        assert!(adoption::apply(&plan, &mut store).is_ok());
        (temporary, store, source, variant, app.inventory().clone())
    }

    fn environment(temporary: &tempfile::TempDir) -> AppEnvironment {
        AppEnvironment::new(
            temporary.path().join("home"),
            temporary.path().join("data"),
            "",
        )
    }

    fn entry(path: &str, bytes: &[u8]) -> OriginEntry {
        OriginEntry {
            path: path.into(),
            executable: false,
            bytes: bytes.into(),
        }
    }

    fn local(files: &[(&str, &[u8])]) -> DirectoryManifest {
        baseline_from_regular_entries_with_directory_modes(
            &files
                .iter()
                .map(|(path, bytes)| (PathBuf::from(path), false, bytes.to_vec()))
                .collect::<Vec<_>>(),
            &BTreeMap::new(),
        )
        .unwrap()
    }

    #[test]
    fn preserves_local_notice_missing_from_origin() {
        let local = local(&[
            ("SKILL.md", b"---\nname: demo\ndescription: old\n---\n"),
            ("LICENSE", b"keep me\n"),
        ]);
        let final_manifest = final_manifest(
            &local,
            &[entry(
                "SKILL.md",
                b"---\nname: demo\ndescription: new\n---\n",
            )],
            &[],
            "demo",
        )
        .unwrap();
        assert!(regular_files(&final_manifest.manifest).contains_key(Path::new("LICENSE")));
    }

    #[test]
    fn blocks_conflicting_notice() {
        let local = local(&[
            ("SKILL.md", b"---\nname: demo\ndescription: old\n---\n"),
            ("NOTICE", b"local\n"),
        ]);
        let error = final_manifest(
            &local,
            &[
                entry("SKILL.md", b"---\nname: demo\ndescription: new\n---\n"),
                entry("NOTICE", b"origin\n"),
            ],
            &[],
            "demo",
        )
        .unwrap_err();
        assert!(error.message.contains("conflicts"));
    }

    #[test]
    fn adds_applicable_ancestor_notice_and_blocks_content_conflicts() {
        let local = local(&[("SKILL.md", b"---\nname: demo\ndescription: old\n---\n")]);
        let planned = final_manifest(
            &local,
            &[entry(
                "SKILL.md",
                b"---\nname: demo\ndescription: new\n---\n",
            )],
            &[entry("LICENSE", b"upstream notice\n")],
            "demo",
        )
        .unwrap();
        assert!(regular_files(&planned.manifest).contains_key(Path::new("LICENSE")));
        let error = final_manifest(
            &local,
            &[
                entry("SKILL.md", b"---\nname: demo\ndescription: new\n---\n"),
                entry("LICENSE", b"selected\n"),
            ],
            &[entry("LICENSE", b"ancestor\n")],
            "demo",
        )
        .unwrap_err();
        assert!(error.message.contains("ancestor notice conflicts"));
    }

    #[test]
    fn rejects_unsafe_candidate_before_planning() {
        let local = local(&[("SKILL.md", b"---\nname: demo\ndescription: old\n---\n")]);
        let error = final_manifest(
            &local,
            &[
                entry("SKILL.md", b"---\nname: demo\ndescription: new\n---\n"),
                entry("../escape", b"bad"),
            ],
            &[],
            "demo",
        )
        .unwrap_err();
        assert!(error.message.contains("unsafe"));
    }

    #[test]
    fn modified_local_content_refuses_before_any_origin_fetch() {
        let (temporary, store, source, variant, inventory) = adopted_fixture();
        fs::write(
            temporary.path().join("source/skills/demo/local-change"),
            "changed",
        )
        .unwrap();
        let failure = prepare(
            &source,
            variant,
            &store,
            &inventory,
            &environment(&temporary),
        )
        .unwrap_err();
        assert!(
            failure
                .message
                .contains("differs from its adopted baseline")
        );
        assert!(!temporary.path().join("data/vendored-origin-cache").exists());
    }

    #[test]
    fn incomplete_inventory_blocks_without_degrading_metadata() {
        let (temporary, store, source, variant, _) = adopted_fixture();
        let agents = crate::agents::detect_agents(&AppEnvironment::new(
            temporary.path().join("other-home"),
            temporary.path().join("other-data"),
            "",
        ));
        let incomplete = InventorySnapshot::not_scanned(&agents, RegistryAvailability::Readable);
        let failure = prepare(
            &source,
            variant,
            &store,
            &incomplete,
            &environment(&temporary),
        )
        .unwrap_err();
        assert!(failure.message.contains("Installation scan is incomplete"));
        assert_eq!(failure.metadata, MetadataAvailability::Available);
    }

    #[test]
    fn unavailable_local_observation_does_not_degrade_metadata() {
        let failure = observation_failure(crate::provenance::ObservationFailure::Unavailable(
            "cannot read selected skill".into(),
        ));
        assert_eq!(failure.metadata, MetadataAvailability::Available);
    }

    #[test]
    fn physical_path_drift_after_prepare_refuses_before_fetch() {
        let (temporary, store, source, variant, inventory) = adopted_fixture();
        let request = prepare(
            &source,
            variant,
            &store,
            &inventory,
            &environment(&temporary),
        )
        .unwrap();
        let skill = temporary.path().join("source/skills/demo");
        fs::rename(&skill, temporary.path().join("moved-demo")).unwrap();
        fs::create_dir(&skill).unwrap();
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: demo\ndescription: Fixture\n---\n",
        )
        .unwrap();
        let failure = recheck_request(&request, &temporary.path().join("data")).unwrap_err();
        assert!(
            failure
                .message
                .contains("changed after the update check began")
        );
        assert!(!temporary.path().join("data/vendored-origin-cache").exists());
    }

    #[cfg(unix)]
    #[test]
    fn affected_installation_drift_after_prepare_blocks_preview() {
        use std::os::unix::fs::symlink;
        let (temporary, store, source, variant, _) = adopted_fixture();
        let environment = environment(&temporary);
        let root = environment
            .home_dir
            .join(crate::agents::adapter(crate::AgentKind::Codex).native_skill_root());
        fs::create_dir_all(&root).unwrap();
        let installed = root.join("demo");
        symlink(temporary.path().join("source/skills/demo"), &installed).unwrap();
        let agents = crate::agents::detect_agents(&environment);
        let inventory = crate::inventory::scan_installations(
            &agents,
            std::slice::from_ref(&source),
            RegistryAvailability::Readable,
        );
        let request = prepare(&source, variant, &store, &inventory, &environment).unwrap();
        fs::remove_file(installed).unwrap();
        let failure =
            recheck_affected_installations(&request, &temporary.path().join("data")).unwrap_err();
        assert!(failure.message.contains("Affected installations changed"));
    }

    #[cfg(unix)]
    #[test]
    fn complete_check_pins_a_local_origin_and_leaves_source_metadata_and_links_unchanged() {
        use std::{ffi::OsString, os::unix::fs::symlink};
        let (temporary, store, source, variant, _) = adopted_fixture();
        let source_root = temporary.path().join("source");
        let remote = temporary.path().join("remote.git");
        let candidate = temporary.path().join("candidate");
        for (directory, arguments) in [
            (
                temporary.path(),
                vec!["init", "--bare", remote.to_str().unwrap()],
            ),
            (
                &source_root,
                vec!["remote", "add", "origin", remote.to_str().unwrap()],
            ),
            (&source_root, vec!["push", "--quiet", "origin", "main"]),
            (
                temporary.path(),
                vec![
                    "clone",
                    "--quiet",
                    remote.to_str().unwrap(),
                    candidate.to_str().unwrap(),
                ],
            ),
        ] {
            let output = Command::new("git")
                .arg("-C")
                .arg(directory)
                .args(arguments)
                .output()
                .unwrap();
            assert!(output.status.success(), "{:?}", output);
        }
        fs::write(candidate.join("LICENSE"), "upstream license\n").unwrap();
        fs::write(
            candidate.join("skills/demo/SKILL.md"),
            "---\nname: demo\ndescription: Updated\n---\n",
        )
        .unwrap();
        for arguments in [
            vec!["add", "."],
            vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
                "commit",
                "-m",
                "update",
            ],
            vec!["push", "--quiet", "origin", "main"],
        ] {
            let output = Command::new("git")
                .arg("-C")
                .arg(&candidate)
                .args(arguments)
                .output()
                .unwrap();
            assert!(output.status.success(), "{:?}", output);
        }
        let revision = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(&candidate)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        let home = temporary.path().join("home");
        let codex = home.join(crate::agents::adapter(crate::AgentKind::Codex).native_skill_root());
        let claude =
            home.join(crate::agents::adapter(crate::AgentKind::ClaudeCode).native_skill_root());
        fs::create_dir_all(&codex).unwrap();
        fs::create_dir_all(&claude).unwrap();
        let target = source_root.join("skills/demo");
        symlink(&target, codex.join("demo")).unwrap();
        symlink(&target, claude.join("demo")).unwrap();
        let environment = AppEnvironment::new(&home, temporary.path().join("data"), "");
        let agents = crate::agents::detect_agents(&environment);
        let inventory = crate::inventory::scan_installations(
            &agents,
            std::slice::from_ref(&source),
            RegistryAvailability::Readable,
        );
        let request = prepare(&source, variant.clone(), &store, &inventory, &environment).unwrap();
        let source_skill = fs::read(source_root.join("skills/demo/SKILL.md")).unwrap();
        let source_head = Command::new("git")
            .arg("-C")
            .arg(&source_root)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout;
        let source_index = Command::new("git")
            .arg("-C")
            .arg(&source_root)
            .args(["status", "--porcelain"])
            .output()
            .unwrap()
            .stdout;
        let before_record = store
            .origin_record(
                variant.source_id(),
                variant.catalog_relative_path(),
                variant.variant_relative_path(),
            )
            .unwrap();
        let config = temporary.path().join("rewrite.gitconfig");
        fs::write(
            &config,
            format!(
                "[url \"file://{}\"]\n\tinsteadOf = https://github.com/example/upstream\n",
                remote.display()
            ),
        )
        .unwrap();
        crate::git::TEST_GIT_CONFIG_GLOBAL
            .with(|value| *value.borrow_mut() = Some(OsString::from(config)));
        let preview = check(
            request,
            &temporary.path().join("data"),
            &AtomicBool::new(false),
            &Mutex::new(None),
        )
        .unwrap()
        .unwrap();
        crate::git::TEST_GIT_CONFIG_GLOBAL.with(|value| *value.borrow_mut() = None);
        assert!(preview.lines().iter().any(|line| line.contains(&revision)));
        assert!(
            preview
                .lines()
                .iter()
                .any(|line| line.contains(&codex.join("demo").display().to_string()))
        );
        assert!(
            preview
                .lines()
                .iter()
                .any(|line| line.contains(&claude.join("demo").display().to_string()))
        );
        assert!(!preview.is_noop());
        assert_eq!(
            fs::read(source_root.join("skills/demo/SKILL.md")).unwrap(),
            source_skill
        );
        assert_eq!(
            Command::new("git")
                .arg("-C")
                .arg(&source_root)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
            source_head
        );
        assert_eq!(
            Command::new("git")
                .arg("-C")
                .arg(&source_root)
                .args(["status", "--porcelain"])
                .output()
                .unwrap()
                .stdout,
            source_index
        );
        assert_eq!(
            store
                .origin_record(
                    variant.source_id(),
                    variant.catalog_relative_path(),
                    variant.variant_relative_path()
                )
                .unwrap(),
            before_record
        );
        assert_eq!(fs::read_link(codex.join("demo")).unwrap(), target);
        assert_eq!(fs::read_link(claude.join("demo")).unwrap(), target);
    }

    #[test]
    fn rejects_case_and_file_descendant_conflicts() {
        let local = local(&[("SKILL.md", b"---\nname: demo\ndescription: old\n---\n")]);
        let case_error = final_manifest(
            &local,
            &[
                entry("SKILL.md", b"---\nname: demo\ndescription: new\n---\n"),
                entry("Readme", b"one"),
                entry("README", b"two"),
            ],
            &[],
            "demo",
        )
        .unwrap_err();
        assert!(case_error.message.contains("case-conflicting"));
        let tree_error = final_manifest(
            &local,
            &[
                entry("SKILL.md", b"---\nname: demo\ndescription: new\n---\n"),
                entry("guide", b"file"),
                entry("guide/readme", b"impossible"),
            ],
            &[],
            "demo",
        )
        .unwrap_err();
        assert!(tree_error.message.contains("file and descendant"));
    }

    #[test]
    fn notice_rule_matches_origin_license_and_hyphen_forms() {
        for name in [
            "LICENCE",
            "license-apache",
            "NOTICE-third-party",
            "ATTRIBUTION.md",
        ] {
            assert!(is_notice(Path::new(name)), "{name}");
        }
        assert!(!is_notice(Path::new("notices")));
    }

    #[cfg(unix)]
    #[test]
    fn executable_only_change_is_disclosed_in_preview() {
        let local = local(&[("SKILL.md", b"---\nname: demo\ndescription: same\n---\n")]);
        let planned = final_manifest(
            &local,
            &[OriginEntry {
                path: PathBuf::from("SKILL.md"),
                executable: true,
                bytes: b"---\nname: demo\ndescription: same\n---\n".to_vec(),
            }],
            &[],
            "demo",
        )
        .unwrap();
        let mut preview = Preview::fixture();
        preview.changes = changes(
            &local,
            &planned.manifest,
            Path::new("/source"),
            Path::new("skill"),
        );
        assert!(
            preview
                .lines()
                .iter()
                .any(|line| line.contains("executable false → true"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn proposed_baseline_matches_materialized_tree_with_modes_and_empty_directories() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let local_root = temporary.path().join("local");
        fs::create_dir_all(local_root.join("nested")).unwrap();
        fs::create_dir(local_root.join("empty")).unwrap();
        fs::write(
            local_root.join("SKILL.md"),
            "---\nname: demo\ndescription: old\n---\n",
        )
        .unwrap();
        fs::write(local_root.join("LICENSE"), "preserve\n").unwrap();
        fs::set_permissions(&local_root, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(local_root.join("nested"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(local_root.join("empty"), fs::Permissions::from_mode(0o755)).unwrap();
        let local = observe_directory_manifest(&local_root).unwrap();
        let planned = final_manifest(
            &local,
            &[
                entry("SKILL.md", b"---\nname: demo\ndescription: new\n---\n"),
                entry("nested/readme", b"upstream\n"),
                entry("nested.txt", b"sibling\n"),
            ],
            &[],
            "demo",
        )
        .unwrap();

        let materialized = temporary.path().join("materialized");
        fs::create_dir_all(materialized.join("nested")).unwrap();
        fs::create_dir(materialized.join("empty")).unwrap();
        fs::write(
            materialized.join("SKILL.md"),
            "---\nname: demo\ndescription: new\n---\n",
        )
        .unwrap();
        fs::write(materialized.join("nested/readme"), "upstream\n").unwrap();
        fs::write(materialized.join("nested.txt"), "sibling\n").unwrap();
        fs::write(materialized.join("LICENSE"), "preserve\n").unwrap();
        fs::set_permissions(&materialized, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(
            materialized.join("nested"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        fs::set_permissions(
            materialized.join("empty"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert_eq!(
            planned.manifest.baseline,
            observe_directory_hash(&materialized).unwrap()
        );
    }
}
