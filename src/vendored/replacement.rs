//! Descriptor-bound staging and replacement for a checked vendored skill.
//!
//! This deliberately has a narrower job than a general tree synchronizer.
//! It materializes the previewed manifest on the destination volume, proves that
//! materialization hashes to the advertised baseline, and changes regular
//! files one at a time.  It never removes a skill root, never follows a link,
//! and leaves every displaced old file in staging as a reportable residue.
//! Keeping those residues is intentional: a recursive cleanup would add a
//! second, much broader destructive boundary to the update operation.

use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{CString, OsStr},
    fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

use crate::provenance::{
    DirectoryManifest, ManifestEntry, ManifestEntryKind, observe_directory_manifest,
};

/// A materialized candidate tree, held until the confirmed replacement runs.
#[derive(Debug)]
pub(crate) struct Stage {
    skill_root: PathBuf,
    staging_root: PathBuf,
    expected: DirectoryManifest,
    /// The directories every mutation is relative to.  The pathnames remain
    /// only for disclosure and revalidation; they are never used as mutation
    /// targets after staging begins.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    skill_directory: fs::File,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    skill_parent: fs::File,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    skill_parent_path: PathBuf,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    staging_directory: fs::File,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    staging_parent: fs::File,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    staging_name: CString,
}

/// One failure is reported with its absolute target, without concealing work
/// completed before it.  The caller must rescan and distinguish this partial
/// filesystem outcome from any later provenance write.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OperationFailure {
    pub(crate) path: PathBuf,
    pub(crate) detail: String,
}

/// The observable outcome of a replacement attempt.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ReplacementReport {
    pub(crate) completed: Vec<PathBuf>,
    pub(crate) failed: Vec<OperationFailure>,
    pub(crate) unattempted: Vec<PathBuf>,
    /// Old files and any recovery tails that were intentionally retained.
    pub(crate) residual_paths: Vec<PathBuf>,
    wrote: bool,
}

enum FileOutcome {
    Changed(Option<PathBuf>),
}

struct FileError {
    detail: String,
    wrote: bool,
    residue: Option<PathBuf>,
}

impl From<String> for FileError {
    fn from(detail: String) -> Self {
        Self {
            detail,
            wrote: false,
            residue: None,
        }
    }
}

fn record_file_error(report: &mut ReplacementReport, path: PathBuf, error: FileError) {
    if error.wrote {
        report.wrote = true;
    }
    if let Some(residue) = error.residue {
        report.residual_paths.push(residue);
    }
    report.failed.push(OperationFailure {
        path,
        detail: error.detail,
    });
}

impl ReplacementReport {
    /// All requested file operations either completed or were already equal.
    pub(crate) fn success(&self) -> bool {
        self.failed.is_empty() && self.unattempted.is_empty()
    }

    /// At least one destination entry changed during this run.
    pub(crate) fn changed(&self) -> bool {
        self.wrote
    }

    /// Stable, absolute-path reporting for the confirmation result dialog.
    pub(crate) fn lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "Completed filesystem operations: {}",
            self.completed.len()
        )];
        for path in &self.completed {
            lines.push(format!("completed: {}", path.display()));
        }
        for failure in &self.failed {
            lines.push(format!(
                "failed: {} ({})",
                failure.path.display(),
                failure.detail
            ));
        }
        for path in &self.unattempted {
            lines.push(format!("not attempted: {}", path.display()));
        }
        for path in &self.residual_paths {
            lines.push(format!("retained residue: {}", path.display()));
        }
        lines
    }
}

impl Stage {
    /// Create a private, unique staging tree beside `skill_root` and prove it
    /// contains exactly `expected`. `staging_root` is rooted in a separately
    /// pinned parent but must share the selected skill's filesystem and may
    /// never fall inside the selected skill.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn prepare(
        skill_root: &Path,
        staging_root: &Path,
        expected: &DirectoryManifest,
    ) -> Result<Self, String> {
        validate_manifest(expected)?;
        let skill_parent = skill_root
            .parent()
            .ok_or("the selected skill has no parent directory")?;
        if staging_root == skill_root {
            return Err("the staging directory must not be the selected skill directory".into());
        }
        let staging_parent_path = staging_root
            .parent()
            .ok_or("the staging directory has no parent")?;
        let canonical_skill = fs::canonicalize(skill_root).map_err(|error| {
            format!(
                "cannot resolve selected skill {}: {error}",
                skill_root.display()
            )
        })?;
        let canonical_staging_parent = fs::canonicalize(staging_parent_path).map_err(|error| {
            format!(
                "cannot resolve staging parent {}: {error}",
                staging_parent_path.display()
            )
        })?;
        if canonical_staging_parent.starts_with(&canonical_skill) {
            return Err(
                "the private staging directory must not be inside the selected skill".into(),
            );
        }
        let skill_directory = open_root(skill_root)?;
        let skill_parent_handle = open_root(skill_parent)?;
        let staging_parent = open_root(staging_parent_path)?;
        same_filesystem_handles(&staging_parent, &skill_directory)?;
        if !same_directory_path(&skill_directory, skill_root)
            || !same_directory_path(&skill_parent_handle, skill_parent)
            || !same_directory_path(&staging_parent, staging_parent_path)
        {
            return Err(
                "the selected skill or its parent changed while staging was being prepared".into(),
            );
        }
        let staging_name = entry_name(
            staging_root
                .file_name()
                .ok_or("the staging path has no file name")?,
        )?;
        mkdir_in(&staging_parent, &staging_name).map_err(|error| {
            format!(
                "cannot create private staging directory {}: {error}",
                staging_root.display()
            )
        })?;
        assert_raw_entry(&staging_parent, &staging_name, true)?;
        let staging_directory = open_dir_in(&staging_parent, &staging_name).map_err(|error| {
            format!(
                "cannot pin private staging directory {}: {error}",
                staging_root.display()
            )
        })?;
        let result = materialize(&staging_directory, expected).and_then(|()| {
            if !same_directory_path(&staging_directory, staging_root) {
                return Err("the private staging directory changed after creation".into());
            }
            let observed =
                observe_directory_manifest(staging_root).map_err(|error| error.to_string())?;
            if observed.baseline != expected.baseline {
                return Err("the staged candidate did not match the previewed baseline".into());
            }
            Ok(())
        });
        if let Err(error) = result {
            // Do not recursively delete an unsuccessfully created tree.  It is
            // private, named by the caller, and can be inspected or removed by
            // an explicit future maintenance operation.
            return Err(format!(
                "{error}; staging remains at {}",
                staging_root.display()
            ));
        }
        Ok(Self {
            skill_root: skill_root.to_path_buf(),
            staging_root: staging_root.to_path_buf(),
            expected: expected.clone(),
            skill_directory,
            skill_parent: skill_parent_handle,
            skill_parent_path: skill_parent.to_path_buf(),
            staging_directory,
            staging_parent,
            staging_name,
        })
    }

    /// Platforms without descriptor-bound rename primitives refuse before
    /// creating a staging directory.  Falling back to `rename` would permit an
    /// unexpected pathname occupant to be overwritten.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(crate) fn prepare(
        _skill_root: &Path,
        _staging_root: &Path,
        _expected: &DirectoryManifest,
    ) -> Result<Self, String> {
        Err("guarded vendored replacement requires Linux or macOS atomic rename support".into())
    }

    /// Apply only entries proven by `original`.  Each operation reopens both
    /// parents without following links and re-reads the current regular file
    /// before changing its directory entry.  Directories are never removed:
    /// local empty directories and all parent structure are retained.
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub(crate) fn apply(&mut self, original: &DirectoryManifest) -> ReplacementReport {
        self.apply_with_guard(original, || Ok(()))
    }

    /// As [`Self::apply`], but runs the caller's source, checkout, and
    /// installation guard immediately before each mutation. The caller must
    /// not use the whole-directory baseline as that guard after the first
    /// write: this executor has intentionally changed it by then.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn apply_with_guard(
        &mut self,
        original: &DirectoryManifest,
        mut guard: impl FnMut() -> Result<(), String>,
    ) -> ReplacementReport {
        let report = (|| {
            let mut report = ReplacementReport::default();
            if !self.held_paths_still_named() {
                report.failed.push(OperationFailure {
                path: self.skill_root.clone(),
                detail: "the selected skill or its private staging directory changed after confirmation; no replacement was attempted".into(),
            });
                return report;
            }
            let current = match observe_directory_manifest(&self.skill_root) {
                Ok(manifest) if manifest.baseline == original.baseline => manifest,
                Ok(_) => {
                    report.failed.push(OperationFailure {
                    path: self.skill_root.clone(),
                    detail: "the selected skill changed after confirmation; no replacement was attempted".into(),
                });
                    return report;
                }
                Err(error) => {
                    report.failed.push(OperationFailure {
                        path: self.skill_root.clone(),
                        detail: error.to_string(),
                    });
                    return report;
                }
            };
            let old = regular_files(&current);
            let desired = regular_files(&self.expected);
            let old_directories = directories(&current);
            let desired_directories = directories(&self.expected);
            // A new upstream subtree may replace a formerly regular local file at
            // its root. Retain that old file first, then create the directory; a
            // plain mkdir before this removal would reject the valid transition.
            let transitions = old
                .keys()
                .filter(|path| desired_directories.contains_key(*path))
                .cloned()
                .collect::<Vec<_>>();
            let mut transitioned = BTreeSet::new();
            for (index, relative) in transitions.iter().enumerate() {
                let path = self.skill_root.join(relative);
                if !self.held_paths_still_named() {
                    report.failed.push(OperationFailure { path: path.clone(), detail: "the selected skill or its private staging directory moved before a file-to-directory transition could be applied".into() });
                    report.unattempted.extend(
                        transitions[index + 1..]
                            .iter()
                            .map(|tail| self.skill_root.join(tail)),
                    );
                    return report;
                }
                if let Err(detail) = guard() {
                    report.failed.push(OperationFailure {
                        path: path.clone(),
                        detail: format!("a required replacement guard no longer holds: {detail}"),
                    });
                    report.unattempted.extend(
                        transitions[index + 1..]
                            .iter()
                            .map(|tail| self.skill_root.join(tail)),
                    );
                    return report;
                }
                match self.remove_file(
                    relative,
                    old.get(relative).expect("transition came from old files"),
                ) {
                    Ok(FileOutcome::Changed(residue)) => {
                        report.completed.push(path);
                        report.wrote = true;
                        if let Some(residue) = residue {
                            report.residual_paths.push(residue);
                        }
                        transitioned.insert(relative.clone());
                    }
                    Err(error) => {
                        if error.wrote {
                            report.wrote = true;
                        }
                        if let Some(residue) = error.residue {
                            report.residual_paths.push(residue);
                        }
                        report.failed.push(OperationFailure {
                            path,
                            detail: error.detail,
                        });
                        return report;
                    }
                }
            }
            let mut added_directories = desired_directories
                .iter()
                .filter(|(path, _)| {
                    !path.as_os_str().is_empty() && !old_directories.contains_key(*path)
                })
                .collect::<Vec<_>>();
            added_directories.sort_by_key(|(path, _)| path.components().count());
            for (index, (relative, _)) in added_directories.iter().enumerate() {
                let path = self.skill_root.join(relative);
                if !self.held_paths_still_named() {
                    report.failed.push(OperationFailure { path: path.clone(), detail: "the selected skill or its private staging directory moved after confirmation; no further replacement was attempted".into() });
                    report.unattempted.extend(
                        added_directories[index + 1..]
                            .iter()
                            .map(|(tail, _)| self.skill_root.join(tail)),
                    );
                    return report;
                }
                if let Err(detail) = guard() {
                    report.failed.push(OperationFailure {
                        path: path.clone(),
                        detail: format!("a required replacement guard no longer holds: {detail}"),
                    });
                    report.unattempted.extend(
                        added_directories[index + 1..]
                            .iter()
                            .map(|(tail, _)| self.skill_root.join(tail)),
                    );
                    return report;
                }
                if let Err(error) = self.create_directory(relative) {
                    record_file_error(&mut report, path.clone(), error);
                    report.unattempted.extend(
                        added_directories[index + 1..]
                            .iter()
                            .map(|(tail, _)| self.skill_root.join(tail)),
                    );
                    return report;
                }
                report.completed.push(path);
                report.wrote = true;
            }
            let mut paths = old
                .keys()
                .chain(desired.keys())
                .cloned()
                .collect::<Vec<_>>();
            paths.sort();
            paths.dedup();

            for (index, relative) in paths.iter().enumerate() {
                let path = self.skill_root.join(relative);
                if transitioned.contains(relative) {
                    continue;
                }
                if matches!(
                    (old.get(relative), desired.get(relative)),
                    (Some(before), Some(after)) if same_file(before, after)
                ) {
                    continue;
                }
                if !self.held_paths_still_named() {
                    report.failed.push(OperationFailure {
                    path: path.clone(),
                    detail: "the selected skill or its private staging directory moved after confirmation; no further replacement was attempted".into(),
                });
                    report.unattempted.extend(
                        paths[index + 1..]
                            .iter()
                            .map(|tail| self.skill_root.join(tail)),
                    );
                    break;
                }
                if let Err(detail) = guard() {
                    report.failed.push(OperationFailure {
                        path: path.clone(),
                        detail: format!("a required replacement guard no longer holds: {detail}"),
                    });
                    report.unattempted.extend(
                        paths[index + 1..]
                            .iter()
                            .map(|tail| self.skill_root.join(tail)),
                    );
                    break;
                }
                let outcome = match (old.get(relative), desired.get(relative)) {
                    (Some(before), Some(after)) => self.replace_file(relative, before, after),
                    (None, Some(after)) => self.add_file(relative, after),
                    (Some(before), None) => self.remove_file(relative, before),
                    (None, None) => unreachable!(),
                };
                match outcome {
                    Ok(FileOutcome::Changed(residue)) => {
                        report.completed.push(path);
                        report.wrote = true;
                        if let Some(residue) = residue {
                            report.residual_paths.push(residue);
                        }
                    }
                    Err(error) => {
                        if error.wrote {
                            report.wrote = true;
                        }
                        if let Some(residue) = error.residue {
                            report.residual_paths.push(residue);
                        }
                        report.failed.push(OperationFailure {
                            path: path.clone(),
                            detail: error.detail,
                        });
                        report.unattempted.extend(
                            paths[index + 1..]
                                .iter()
                                .map(|tail| self.skill_root.join(tail)),
                        );
                        break;
                    }
                }
            }
            // A directory without its search bit cannot be used to materialize a
            // child. Apply restrictive expected directory modes only after all
            // children and files have been created, deepest first.
            for (relative, entry) in added_directories.into_iter().rev() {
                if entry.executable {
                    continue;
                }
                let path = self.skill_root.join(relative);
                if !self.held_paths_still_named() {
                    report.failed.push(OperationFailure { path, detail: "the selected skill or its private staging directory moved before final directory permissions could be applied".into() });
                    return report;
                }
                if let Err(detail) = guard() {
                    report.failed.push(OperationFailure {
                        path,
                        detail: format!("a required replacement guard no longer holds: {detail}"),
                    });
                    return report;
                }
                if let Err(detail) = self.set_directory_mode(relative, false) {
                    report.failed.push(OperationFailure { path, detail });
                    return report;
                }
                report.completed.push(path);
                report.wrote = true;
            }
            report
        })();
        self.finalize_report(report, original)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn finalize_report(
        &self,
        mut report: ReplacementReport,
        original: &DirectoryManifest,
    ) -> ReplacementReport {
        let mut remaining = planned_operation_paths(&self.skill_root, original, &self.expected)
            .into_iter()
            .fold(BTreeMap::<PathBuf, usize>::new(), |mut counts, path| {
                *counts.entry(path).or_default() += 1;
                counts
            });
        for path in report
            .completed
            .iter()
            .chain(report.failed.iter().map(|failure| &failure.path))
        {
            if let Some(count) = remaining.get_mut(path) {
                *count = count.saturating_sub(1);
            }
        }
        report.unattempted = remaining
            .into_iter()
            .flat_map(|(path, count)| std::iter::repeat_n(path, count))
            .collect();
        report
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(crate) fn apply(&mut self, _original: &DirectoryManifest) -> ReplacementReport {
        ReplacementReport {
            failed: vec![OperationFailure {
                path: self.skill_root.clone(),
                detail:
                    "guarded vendored replacement requires Linux or macOS atomic rename support"
                        .into(),
            }],
            ..ReplacementReport::default()
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(crate) fn apply_with_guard(
        &mut self,
        _original: &DirectoryManifest,
        _guard: impl FnMut() -> Result<(), String>,
    ) -> ReplacementReport {
        self.apply(_original)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn held_paths_still_named(&self) -> bool {
        same_directory_path(&self.skill_directory, &self.skill_root)
            && same_directory_path(&self.skill_parent, &self.skill_parent_path)
            && same_directory_path(&self.staging_directory, &self.staging_root)
            && same_directory_path(
                &self.staging_parent,
                self.staging_root.parent().unwrap_or_else(|| Path::new("")),
            )
            && open_dir_in(&self.staging_parent, &self.staging_name)
                .map(|observed| same_directory(&observed, &self.staging_directory))
                .unwrap_or(false)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn replace_file(
        &self,
        relative: &Path,
        before: &ManifestEntry,
        after: &ManifestEntry,
    ) -> Result<FileOutcome, FileError> {
        let (destination_parent, destination_name) =
            pinned_parent_from(&self.skill_directory, relative)?;
        let (staging_parent, staging_name) = pinned_parent_from(&self.staging_directory, relative)?;
        let original_mode = prove_regular(&destination_parent, &destination_name, before)?;
        prove_regular(&staging_parent, &staging_name, after).map_err(|error| {
            format!("the staged candidate changed after it was verified: {error}")
        })?;
        let candidate_mode = replacement_mode(original_mode, after.executable);
        set_entry_mode(&staging_parent, &staging_name, after, candidate_mode).map_err(|error| {
            format!("the staged candidate could not retain destination permissions: {error}")
        })?;
        exchange_between(
            &staging_parent,
            &staging_name,
            &destination_parent,
            &destination_name,
        )
        .map_err(exchange_error)?;
        // The old object is now held in the private staging tree.  Refuse to
        // call it a successful replacement unless it is exactly the object the
        // confirmation guarded.  A mismatch is exchanged back, preserving the
        // arrival at the public name if a second race made restoration unsafe.
        if let Err(mismatch) = prove_regular(&staging_parent, &staging_name, before) {
            let residue = self.staging_root.join(relative);
            if let Err(observed) = prove_regular(&destination_parent, &destination_name, after) {
                return Err(FileError {
                    detail: format!(
                        "the destination changed during replacement ({mismatch}); it no longer holds the staged candidate ({observed}), so it was not exchanged back; retained residue is at {}",
                        residue.display()
                    ),
                    wrote: true,
                    residue: Some(residue),
                });
            }
            match exchange_between(
                &staging_parent,
                &staging_name,
                &destination_parent,
                &destination_name,
            ) {
                Ok(()) => {
                    return Err(FileError::from(format!(
                        "the destination changed during replacement ({mismatch}); it was restored"
                    )));
                }
                Err(restore) => {
                    return Err(FileError {
                        detail: format!(
                            "the destination changed during replacement ({mismatch}) and could not be restored ({restore}); staged residue remains at {}",
                            residue.display()
                        ),
                        wrote: true,
                        residue: Some(residue),
                    });
                }
            }
        }
        Ok(FileOutcome::Changed(Some(self.staging_root.join(relative))))
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn add_file(&self, relative: &Path, after: &ManifestEntry) -> Result<FileOutcome, FileError> {
        let (destination_parent, destination_name) =
            pinned_parent_from(&self.skill_directory, relative)?;
        let (staging_parent, staging_name) = pinned_parent_from(&self.staging_directory, relative)?;
        prove_regular(&staging_parent, &staging_name, after).map_err(|error| {
            format!("the staged candidate changed after it was verified: {error}")
        })?;
        rename_no_replace(
            &staging_parent,
            &staging_name,
            &destination_parent,
            &destination_name,
        )
        .map_err(|error| format!("refused to add over an occupied destination: {error}"))?;
        Ok(FileOutcome::Changed(None))
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn remove_file(
        &self,
        relative: &Path,
        before: &ManifestEntry,
    ) -> Result<FileOutcome, FileError> {
        let (destination_parent, destination_name) =
            pinned_parent_from(&self.skill_directory, relative)?;
        prove_regular(&destination_parent, &destination_name, before)?;
        let backup_name = backup_name(relative)?;
        rename_no_replace(
            &destination_parent,
            &destination_name,
            &self.staging_directory,
            &backup_name,
        )
        .map_err(|error| {
            format!("could not move the proven removed file to its retained backup: {error}")
        })?;
        let backup = self.staging_root.join(os_string_from_cstring(&backup_name));
        // A concurrent replacement cannot be destroyed by the no-clobber
        // move.  Re-read the residue before reporting the removal complete.
        if let Err(error) = prove_regular(&self.staging_directory, &backup_name, before) {
            return Err(restore_mismatched_removal(&backup, &error, || {
                rename_no_replace(
                    &self.staging_directory,
                    &backup_name,
                    &destination_parent,
                    &destination_name,
                )
            }));
        }
        Ok(FileOutcome::Changed(Some(backup)))
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn create_directory(&self, relative: &Path) -> Result<(), FileError> {
        self.create_directory_with(relative, |parent, name| {
            assert_raw_entry(parent, name, true)?;
            let created = open_dir_in(parent, name).map_err(|error| error.to_string())?;
            // Keep the directory searchable for its descendants. Restrictive
            // expected modes are set after the file phase.
            set_directory_mode_handle(&created, true)
        })
    }

    /// The post-`mkdirat` step is injectable for deterministic recovery
    /// tests. Once mkdir succeeds, any later observation or chmod failure is
    /// a live partial mutation, even if the new directory cannot be verified.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn create_directory_with(
        &self,
        relative: &Path,
        after_mkdir: impl FnOnce(&fs::File, &CString) -> Result<(), String>,
    ) -> Result<(), FileError> {
        let (parent, name) = pinned_parent_from(&self.skill_directory, relative)?;
        mkdir_in(&parent, &name).map_err(|error| {
            format!("refused to create an occupied destination directory: {error}")
        })?;
        after_mkdir(&parent, &name).map_err(|detail| FileError {
            detail,
            wrote: true,
            residue: None,
        })
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn set_directory_mode(&self, relative: &Path, executable: bool) -> Result<(), String> {
        let (parent, name) = pinned_parent_from(&self.skill_directory, relative)?;
        let directory = open_dir_in(&parent, &name).map_err(|error| error.to_string())?;
        set_directory_mode_handle(&directory, executable)
    }
}

/// A mismatched object was moved from the public path by the no-clobber
/// removal. Try to return it through another no-clobber move; if a second
/// arrival owns that name, retain the first object in staging and state it.
fn restore_mismatched_removal(
    backup: &Path,
    mismatch: &str,
    restore: impl FnOnce() -> io::Result<()>,
) -> FileError {
    match restore() {
        Ok(()) => FileError::from(format!(
            "the moved removal backup no longer matched the confirmed file ({mismatch}); it was restored without overwriting a public path"
        )),
        Err(error) => FileError {
            detail: format!(
                "the moved removal backup no longer matched the confirmed file ({mismatch}) and could not be restored without overwriting a later public arrival ({error}); it remains at {}",
                backup.display()
            ),
            wrote: true,
            residue: Some(backup.to_path_buf()),
        },
    }
}

fn regular_files(manifest: &DirectoryManifest) -> BTreeMap<PathBuf, ManifestEntry> {
    manifest
        .entries
        .iter()
        .filter(|entry| entry.kind == ManifestEntryKind::File)
        .map(|entry| (entry.relative_path.clone(), entry.clone()))
        .collect()
}

fn directories(manifest: &DirectoryManifest) -> BTreeMap<PathBuf, ManifestEntry> {
    manifest
        .entries
        .iter()
        .filter(|entry| entry.kind == ManifestEntryKind::Directory)
        .map(|entry| (entry.relative_path.clone(), entry.clone()))
        .collect()
}

/// Every destination operation the confirmed manifests require. This is
/// computed independently of the phase that stopped, so an early transition
/// or directory failure still names pending file replacements, additions, and
/// removals. A path can occur more than once where a file-to-directory
/// transition has both a retained removal and a new directory operation, or
/// where a restrictive directory mode follows its creation.
fn planned_operation_paths(
    skill_root: &Path,
    original: &DirectoryManifest,
    expected: &DirectoryManifest,
) -> Vec<PathBuf> {
    let old_files = regular_files(original);
    let new_files = regular_files(expected);
    let file_names = old_files
        .keys()
        .chain(new_files.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut paths = file_names
        .into_iter()
        .filter(|path| match (old_files.get(path), new_files.get(path)) {
            (Some(before), Some(after)) => !same_file(before, after),
            _ => true,
        })
        .map(|path| skill_root.join(path))
        .collect::<Vec<_>>();
    let old_directories = directories(original);
    paths.extend(
        directories(expected)
            .into_keys()
            .filter(|path| !path.as_os_str().is_empty() && !old_directories.contains_key(path))
            .flat_map(|path| {
                let absolute = skill_root.join(&path);
                let needs_restrictive_mode = directories(expected)
                    .get(&path)
                    .is_some_and(|entry| !entry.executable);
                std::iter::once(absolute.clone()).chain(needs_restrictive_mode.then_some(absolute))
            }),
    );
    paths.sort();
    paths
}

fn same_file(left: &ManifestEntry, right: &ManifestEntry) -> bool {
    left.executable == right.executable && left.bytes == right.bytes
}

fn validate_manifest(manifest: &DirectoryManifest) -> Result<(), String> {
    for entry in &manifest.entries {
        validate_relative(&entry.relative_path)?;
        match entry.kind {
            ManifestEntryKind::Directory | ManifestEntryKind::File => {}
            ManifestEntryKind::Symlink => {
                return Err("a replacement manifest may not contain symbolic links".into());
            }
        }
    }
    Ok(())
}

fn validate_relative(path: &Path) -> Result<(), String> {
    for component in path.components() {
        match component {
            Component::CurDir if path.as_os_str().is_empty() => {}
            Component::Normal(name) if !name.eq_ignore_ascii_case(".git") => {}
            _ => {
                return Err(format!(
                    "replacement manifest has an unsafe relative path: {}",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn materialize(root: &fs::File, expected: &DirectoryManifest) -> Result<(), String> {
    let mut directories = expected
        .entries
        .iter()
        .filter(|entry| entry.kind == ManifestEntryKind::Directory)
        .collect::<Vec<_>>();
    directories.sort_by_key(|entry| entry.relative_path.components().count());
    for entry in &directories {
        if entry.relative_path.as_os_str().is_empty() {
            continue;
        }
        let (parent, name) = pinned_parent_from(root, &entry.relative_path)?;
        mkdir_in(&parent, &name).map_err(|error| {
            format!(
                "cannot create staged directory {}: {error}",
                entry.relative_path.display()
            )
        })?;
        assert_raw_entry(&parent, &name, true)?;
        // Keep creation parents searchable until every descendant and file is
        // materialized. The advertised modes are applied afterwards.
        set_directory_mode_handle(
            &open_dir_in(&parent, &name).map_err(|error| error.to_string())?,
            true,
        )?;
    }
    for entry in expected
        .entries
        .iter()
        .filter(|entry| entry.kind == ManifestEntryKind::File)
    {
        let (parent, name) = pinned_parent_from(root, &entry.relative_path)?;
        let mut file = create_file_in(&parent, &name).map_err(|error| {
            format!(
                "cannot create staged file {}: {error}",
                entry.relative_path.display()
            )
        })?;
        assert_raw_entry(&parent, &name, false)?;
        file.write_all(&entry.bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| {
                format!(
                    "cannot write staged file {}: {error}",
                    entry.relative_path.display()
                )
            })?;
        set_file_mode_handle(&file, entry.executable)?;
    }
    for entry in directories.into_iter().rev() {
        if entry.relative_path.as_os_str().is_empty() {
            // `root` is the private staging directory itself.  It remains
            // owner-only even if the source directory was more permissive.
            continue;
        }
        let (parent, name) = pinned_parent_from(root, &entry.relative_path)?;
        set_directory_mode_handle(
            &open_dir_in(&parent, &name).map_err(|error| error.to_string())?,
            entry.executable,
        )?;
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn set_directory_mode_handle(file: &fs::File, executable: bool) -> Result<(), String> {
    set_mode_handle(file, executable, "directory")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn set_file_mode_handle(file: &fs::File, executable: bool) -> Result<(), String> {
    set_mode_handle(file, executable, "file")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn set_mode_handle(file: &fs::File, executable: bool, kind: &str) -> Result<(), String> {
    // Staging itself stays private (created as 0700), but content moved into
    // the checkout must retain the ordinary source visibility expected of a
    // skill: 0644 for data and 0755 for executable entries.
    let mode = if executable { 0o755 } else { 0o644 };
    set_exact_mode_handle(file, mode)
        .map_err(|error| format!("cannot set staged {kind} permissions: {error}"))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn set_exact_mode_handle(file: &fs::File, mode: u32) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    if unsafe { libc::fchmod(file.as_raw_fd(), mode as libc::mode_t) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error().to_string())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn same_filesystem_handles(parent: &fs::File, root: &fs::File) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let parent = parent.metadata().map_err(|error| error.to_string())?;
    let root = root.metadata().map_err(|error| error.to_string())?;
    if parent.dev() != root.dev() {
        return Err("the selected skill parent is on a different filesystem".into());
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_root(root: &Path) -> Result<fs::File, String> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)
        .map_err(|error| {
            format!(
                "cannot pin directory {} without following links: {error}",
                root.display()
            )
        })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn pinned_parent_from(root: &fs::File, relative: &Path) -> Result<(fs::File, CString), String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    let name = relative
        .file_name()
        .ok_or("replacement path has no file name")?;
    let mut directory = root.try_clone().map_err(|error| error.to_string())?;
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    for component in parent.components() {
        let Component::Normal(name) = component else {
            return Err("replacement path is not plain".into());
        };
        let name = CString::new(name.as_bytes()).map_err(|_| "replacement path contains NUL")?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(format!(
                "cannot pin replacement parent {}: {}",
                parent.display(),
                io::Error::last_os_error()
            ));
        }
        assert_raw_entry(&directory, &name, true)?;
        directory = unsafe { fs::File::from_raw_fd(fd) };
    }
    let name = CString::new(name.as_bytes()).map_err(|_| "replacement file name contains NUL")?;
    Ok((directory, name))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn entry_name(name: &OsStr) -> Result<CString, String> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(name.as_bytes()).map_err(|_| "replacement path contains NUL".into())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_dir_in(parent: &fs::File, name: &CString) -> io::Result<fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { fs::File::from_raw_fd(fd) })
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn mkdir_in(parent: &fs::File, name: &CString) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn create_file_in(parent: &fs::File, name: &CString) -> io::Result<fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { fs::File::from_raw_fd(fd) })
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn same_directory_path(held: &fs::File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (
        held.metadata(),
        open_root(path).and_then(|file| file.metadata().map_err(|error| error.to_string())),
    ) {
        (Ok(held), Ok(observed)) => held.dev() == observed.dev() && held.ino() == observed.ino(),
        _ => false,
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn same_directory(left: &fs::File, right: &fs::File) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (left.metadata(), right.metadata()) {
        (Ok(left), Ok(right)) => left.dev() == right.dev() && left.ino() == right.ino(),
        _ => false,
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn prove_regular(
    parent: &fs::File,
    name: &CString,
    expected: &ManifestEntry,
) -> Result<u32, String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    assert_raw_entry(parent, name, false)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(format!(
            "cannot open the confirmed file without following links: {}",
            io::Error::last_os_error()
        ));
    }
    let file = unsafe { fs::File::from_raw_fd(fd) };
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.file_type().is_file() {
        return Err("the confirmed path is no longer a regular file".into());
    }
    let maximum = expected
        .bytes
        .len()
        .checked_add(1)
        .ok_or("the expected file size overflowed its verification bound")?;
    if metadata.len() > maximum as u64 {
        return Err("the confirmed file exceeds the previewed byte bound".into());
    }
    let mut bytes = Vec::new();
    file.take(maximum as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > expected.bytes.len() {
        return Err("the confirmed file grew beyond its previewed bytes".into());
    }
    use std::os::unix::fs::PermissionsExt;
    let mode = metadata.permissions().mode() & 0o777;
    let executable = mode & 0o111 != 0;
    if bytes != expected.bytes || executable != expected.executable {
        return Err("the confirmed file bytes or executable bit changed".into());
    }
    Ok(mode)
}

/// Replacements keep the current file's read/write visibility. Git records
/// only executability, so candidate content controls execute bits while the
/// local file supplies the non-execute permissions. An executable candidate
/// gains execute where the existing mode grants read, with owner execute as a
/// minimum for a writeable executable such as a private 0600 script.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn replacement_mode(existing: u32, executable: bool) -> u32 {
    let read_write = existing & 0o666;
    if !executable {
        return read_write;
    }
    let execute_from_read = (existing & 0o444) >> 2;
    read_write | execute_from_read | 0o100
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn set_entry_mode(
    parent: &fs::File,
    name: &CString,
    expected: &ManifestEntry,
    mode: u32,
) -> Result<(), String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(format!(
            "cannot reopen replacement without following links: {}",
            io::Error::last_os_error()
        ));
    }
    let file = unsafe { fs::File::from_raw_fd(fd) };
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.file_type().is_file() {
        return Err("the staged candidate is no longer a regular file".into());
    }
    let maximum = expected
        .bytes
        .len()
        .checked_add(1)
        .ok_or("the expected file size overflowed its verification bound")?;
    if metadata.len() > maximum as u64 {
        return Err("the staged candidate exceeds the previewed byte bound".into());
    }
    let mut bytes = Vec::new();
    file.try_clone()
        .map_err(|error| error.to_string())?
        .take(maximum as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes != expected.bytes {
        return Err("the staged candidate bytes changed before permissions were applied".into());
    }
    use std::os::unix::fs::PermissionsExt;
    if (metadata.permissions().mode() & 0o111 != 0) != expected.executable {
        return Err(
            "the staged candidate executable bit changed before permissions were applied".into(),
        );
    }
    set_exact_mode_handle(&file, mode)
}

/// An `openat` on a normalizing or case-folding filesystem can resolve a
/// different raw spelling. Listing the same pinned parent closes that alias:
/// the observed name must be byte-identical to the previewed one.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn assert_raw_entry(parent: &fs::File, name: &CString, directory: bool) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    let requested = OsStr::from_bytes(name.as_bytes());
    let entries = crate::git::bound_directory_entries(parent, 16_384)
        .map_err(|error| format!("cannot verify filesystem spelling: {error}"))?
        .ok_or("the replacement parent exceeds the supported entry limit")?;
    match entries
        .into_iter()
        .find(|entry| entry.name.as_os_str() == requested)
    {
        Some(entry) if entry.is_directory == directory => Ok(()),
        Some(_) => Err("the replacement path type changed while it was being verified".into()),
        None => Err("the filesystem did not preserve the previewed raw pathname spelling".into()),
    }
}

#[cfg(target_os = "linux")]
fn exchange_between(
    a_dir: &fs::File,
    a: &CString,
    b_dir: &fs::File,
    b: &CString,
) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    if unsafe {
        libc::renameat2(
            a_dir.as_raw_fd(),
            a.as_ptr(),
            b_dir.as_raw_fd(),
            b.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
#[cfg(target_os = "macos")]
fn exchange_between(
    a_dir: &fs::File,
    a: &CString,
    b_dir: &fs::File,
    b: &CString,
) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    if unsafe {
        libc::renameatx_np(
            a_dir.as_raw_fd(),
            a.as_ptr(),
            b_dir.as_raw_fd(),
            b.as_ptr(),
            libc::RENAME_SWAP,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn rename_no_replace(
    a_dir: &fs::File,
    a: &CString,
    b_dir: &fs::File,
    b: &CString,
) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    if unsafe {
        libc::renameat2(
            a_dir.as_raw_fd(),
            a.as_ptr(),
            b_dir.as_raw_fd(),
            b.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
#[cfg(target_os = "macos")]
fn rename_no_replace(
    a_dir: &fs::File,
    a: &CString,
    b_dir: &fs::File,
    b: &CString,
) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    if unsafe {
        libc::renameatx_np(
            a_dir.as_raw_fd(),
            a.as_ptr(),
            b_dir.as_raw_fd(),
            b.as_ptr(),
            libc::RENAME_EXCL,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn exchange_error(error: io::Error) -> String {
    let unsupported = matches!(
        error.raw_os_error(),
        Some(libc::ENOTSUP | libc::EINVAL | libc::ENOSYS)
    );
    if unsupported {
        format!("the filesystem does not support atomic exchange: {error}")
    } else {
        format!("atomic replacement failed: {error}")
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn backup_name(relative: &Path) -> Result<CString, String> {
    use std::os::unix::ffi::OsStrExt;
    let mut hasher = Sha256::new();
    hasher.update(relative.as_os_str().as_bytes());
    let value = format!(".skilled-vendored-removed-{:x}", hasher.finalize()).into_bytes();
    CString::new(value).map_err(|_| "replacement backup name contains NUL".into())
}

/// The exact staging residue a planned removal can leave behind. The caller
/// can disclose it before confirmation. It derives from raw relative bytes;
/// an occupied name refuses the no-clobber move instead of overwriting it.
pub(crate) fn removed_backup_path(staging_root: &Path, relative: &Path) -> PathBuf {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        staging_root.join(os_string_from_cstring(
            &backup_name(relative).expect("relative path was validated"),
        ))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    staging_root.join(".skilled-vendored-removed-unsupported")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn os_string_from_cstring(value: &CString) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt;
    std::ffi::OsString::from_vec(value.as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::baseline_from_regular_entries_with_directory_modes;
    use std::collections::BTreeMap;

    fn manifest(files: &[(&str, bool, &[u8])]) -> DirectoryManifest {
        let mut directories = BTreeMap::new();
        directories.insert(PathBuf::new(), true);
        for (path, _, _) in files {
            let path = Path::new(path);
            for parent in path.ancestors().skip(1) {
                directories.entry(parent.to_path_buf()).or_insert(true);
            }
        }
        baseline_from_regular_entries_with_directory_modes(
            &files
                .iter()
                .map(|(path, executable, bytes)| (PathBuf::from(path), *executable, bytes.to_vec()))
                .collect::<Vec<_>>(),
            &directories,
        )
        .unwrap()
    }

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn nested_replacement_preserves_git_and_retains_displaced_files() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("checkout/skills/demo");
        write(&root.join("SKILL.md"), b"old\n");
        write(&root.join("nested/old.md"), b"old nested\n");
        write(
            &temporary.path().join("checkout/.git/sentinel"),
            b"metadata",
        );
        let original = observe_directory_manifest(&root).unwrap();
        let expected = manifest(&[
            ("SKILL.md", false, b"new\n"),
            ("nested/new.md", true, b"new nested\n"),
        ]);
        // Stage on the selected checkout's volume but outside both the nested
        // skill and its immediate parent; this is the production layout.
        let stage_path = temporary.path().join(".skilled-stage-test");
        let mut stage = Stage::prepare(&root, &stage_path, &expected).unwrap();
        let report = stage.apply(&original);
        assert!(report.failed.is_empty(), "{report:?}");
        assert_eq!(fs::read(root.join("SKILL.md")).unwrap(), b"new\n");
        assert_eq!(
            fs::read(root.join("nested/new.md")).unwrap(),
            b"new nested\n"
        );
        assert!(!root.join("nested/old.md").exists());
        assert_eq!(
            fs::read(temporary.path().join("checkout/.git/sentinel")).unwrap(),
            b"metadata"
        );
        assert_eq!(
            observe_directory_manifest(&root).unwrap().baseline,
            expected.baseline
        );
        assert!(
            report
                .residual_paths
                .iter()
                .all(|path| path.starts_with(&stage_path))
        );
        assert!(
            report
                .residual_paths
                .iter()
                .any(|path| path == &removed_backup_path(&stage_path, Path::new("nested/old.md")))
        );
    }

    #[test]
    fn repository_root_skill_keeps_git_sentinel() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("checkout");
        write(&root.join("SKILL.md"), b"old\n");
        write(&root.join(".git/HEAD"), b"ref: refs/heads/main\n");
        let original = observe_directory_manifest(&root).unwrap();
        let expected = manifest(&[("SKILL.md", false, b"new\n")]);
        let stage_path = temporary.path().join(".skilled-stage-root");
        let mut stage = Stage::prepare(&root, &stage_path, &expected).unwrap();
        let report = stage.apply(&original);
        assert!(report.failed.is_empty(), "{report:?}");
        assert_eq!(fs::read(root.join("SKILL.md")).unwrap(), b"new\n");
        assert_eq!(
            fs::read(root.join(".git/HEAD")).unwrap(),
            b"ref: refs/heads/main\n"
        );
    }

    #[test]
    fn stale_content_refuses_before_any_write() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("skill");
        write(&root.join("SKILL.md"), b"old\n");
        let original = observe_directory_manifest(&root).unwrap();
        let expected = manifest(&[("SKILL.md", false, b"new\n")]);
        let mut stage = Stage::prepare(&root, &temporary.path().join(".stage"), &expected).unwrap();
        write(&root.join("SKILL.md"), b"someone else\n");
        let report = stage.apply(&original);
        assert_eq!(fs::read(root.join("SKILL.md")).unwrap(), b"someone else\n");
        assert_eq!(report.completed, Vec::<PathBuf>::new());
        assert_eq!(report.failed.len(), 1);
    }

    #[test]
    fn staging_inside_selected_skill_is_refused() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("skill");
        write(&root.join("SKILL.md"), b"old\n");
        let expected = manifest(&[("SKILL.md", false, b"new\n")]);
        let error = Stage::prepare(&root, &root.join(".skilled-stage"), &expected).unwrap_err();
        assert!(error.contains("must not be inside"), "{error}");
    }

    #[test]
    fn occupied_stage_and_missing_staged_file_refuse_without_overwrite() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("skill");
        write(&root.join("a.md"), b"old\n");
        let original = observe_directory_manifest(&root).unwrap();
        let expected = manifest(&[("a.md", false, b"new\n"), ("z.md", false, b"add\n")]);
        let occupied = temporary.path().join(".occupied");
        fs::create_dir(&occupied).unwrap();
        assert!(Stage::prepare(&root, &occupied, &expected).is_err());
        let stage_path = temporary.path().join(".stage");
        let mut stage = Stage::prepare(&root, &stage_path, &expected).unwrap();
        fs::remove_file(stage_path.join("z.md")).unwrap();
        let report = stage.apply(&original);
        assert_eq!(fs::read(root.join("a.md")).unwrap(), b"new\n");
        assert_eq!(report.completed, vec![root.join("a.md")]);
        assert_eq!(report.failed.len(), 1);
        assert!(report.failed[0].path.ends_with("z.md"));
    }

    #[test]
    fn guard_runs_before_each_file_mutation_and_stops_partial_apply() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("skill");
        write(&root.join("a.md"), b"old a\n");
        write(&root.join("z.md"), b"old z\n");
        let original = observe_directory_manifest(&root).unwrap();
        let expected = manifest(&[("a.md", false, b"new a\n"), ("z.md", false, b"new z\n")]);
        let mut stage = Stage::prepare(&root, &temporary.path().join(".stage"), &expected).unwrap();
        let mut calls = 0;
        let report = stage.apply_with_guard(&original, || {
            calls += 1;
            (calls == 1)
                .then_some(())
                .ok_or_else(|| "checkout HEAD changed".into())
        });
        assert_eq!(fs::read(root.join("a.md")).unwrap(), b"new a\n");
        assert_eq!(fs::read(root.join("z.md")).unwrap(), b"old z\n");
        assert_eq!(calls, 2);
        assert_eq!(report.completed, vec![root.join("a.md")]);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.unattempted.len(), 0);
    }

    #[test]
    fn tampered_stage_refuses_before_replacing_source_file() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("skill");
        write(&root.join("SKILL.md"), b"old\n");
        let original = observe_directory_manifest(&root).unwrap();
        let expected = manifest(&[("SKILL.md", false, b"new\n")]);
        let stage_path = temporary.path().join(".stage");
        let mut stage = Stage::prepare(&root, &stage_path, &expected).unwrap();
        fs::write(stage_path.join("SKILL.md"), b"tampered\n").unwrap();
        let report = stage.apply(&original);
        assert_eq!(fs::read(root.join("SKILL.md")).unwrap(), b"old\n");
        assert_eq!(report.completed, Vec::<PathBuf>::new());
        assert_eq!(report.failed.len(), 1);
    }

    #[test]
    fn oversized_staged_file_is_refused_at_the_preview_byte_bound() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("skill");
        write(&root.join("SKILL.md"), b"old\n");
        let original = observe_directory_manifest(&root).unwrap();
        let expected = manifest(&[("SKILL.md", false, b"new\n")]);
        let stage_path = temporary.path().join(".stage");
        let mut stage = Stage::prepare(&root, &stage_path, &expected).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(stage_path.join("SKILL.md"))
            .unwrap()
            .set_len(1024 * 1024)
            .unwrap();
        let report = stage.apply(&original);
        assert_eq!(fs::read(root.join("SKILL.md")).unwrap(), b"old\n");
        assert_eq!(report.completed, Vec::<PathBuf>::new());
        assert_eq!(report.failed.len(), 1);
        assert!(report.failed[0].detail.contains("byte bound"));
    }

    #[test]
    fn newly_created_directory_is_reported_when_later_stage_file_is_tampered() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("skill");
        write(&root.join("SKILL.md"), b"old\n");
        let original = observe_directory_manifest(&root).unwrap();
        let expected = manifest(&[
            ("SKILL.md", false, b"new\n"),
            ("new-directory/added.md", false, b"new file\n"),
        ]);
        let stage_path = temporary.path().join(".stage");
        let mut stage = Stage::prepare(&root, &stage_path, &expected).unwrap();
        fs::write(stage_path.join("new-directory/added.md"), b"tampered\n").unwrap();
        let report = stage.apply(&original);
        assert!(root.join("new-directory").is_dir());
        assert_eq!(fs::read(root.join("SKILL.md")).unwrap(), b"new\n");
        assert!(fs::read(root.join("new-directory/added.md")).is_err());
        assert!(report.completed.contains(&root.join("new-directory")));
        assert!(report.changed());
        assert_eq!(report.failed.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn private_staging_root_keeps_publishable_file_and_directory_modes() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("checkout/skills/demo");
        write(&root.join("SKILL.md"), b"old\n");
        let original = observe_directory_manifest(&root).unwrap();
        let expected = manifest(&[
            ("SKILL.md", false, b"new\n"),
            ("nested/run", true, b"#!/bin/sh\n"),
        ]);
        let stage_path = temporary.path().join(".skilled-stage");
        let mut stage = Stage::prepare(&root, &stage_path, &expected).unwrap();
        assert_eq!(
            fs::metadata(&stage_path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(stage_path.join("SKILL.md"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
        assert_eq!(
            fs::metadata(stage_path.join("nested"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(stage_path.join("nested/run"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        let report = stage.apply(&original);
        assert!(report.success(), "{report:?}");
        assert_eq!(
            fs::metadata(root.join("SKILL.md"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
        assert_eq!(
            fs::metadata(root.join("nested"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(root.join("nested/run"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }

    #[cfg(unix)]
    #[test]
    fn replacement_preserves_existing_read_write_visibility() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("skill");
        write(&root.join("private.md"), b"old private\n");
        write(&root.join("public.md"), b"old public\n");
        fs::set_permissions(root.join("private.md"), fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(root.join("public.md"), fs::Permissions::from_mode(0o644)).unwrap();
        let original = observe_directory_manifest(&root).unwrap();
        let expected = manifest(&[
            ("private.md", false, b"new private\n"),
            ("public.md", false, b"new public\n"),
        ]);
        let mut stage = Stage::prepare(&root, &temporary.path().join(".stage"), &expected).unwrap();
        let report = stage.apply(&original);
        assert!(report.success(), "{report:?}");
        assert_eq!(
            fs::metadata(root.join("private.md"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(root.join("public.md"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
    }

    #[test]
    fn file_to_directory_transition_retains_old_file_before_creating_subtree() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("skill");
        write(&root.join("assets"), b"old flat asset\n");
        let original = observe_directory_manifest(&root).unwrap();
        let expected = manifest(&[("assets/icon.txt", false, b"new nested asset\n")]);
        let stage_path = temporary.path().join(".stage");
        let mut stage = Stage::prepare(&root, &stage_path, &expected).unwrap();
        let report = stage.apply(&original);
        assert!(report.success(), "{report:?}");
        assert_eq!(
            fs::read(root.join("assets/icon.txt")).unwrap(),
            b"new nested asset\n"
        );
        assert_eq!(
            fs::read(removed_backup_path(&stage_path, Path::new("assets"))).unwrap(),
            b"old flat asset\n"
        );
        assert_eq!(
            observe_directory_manifest(&root).unwrap().baseline,
            expected.baseline
        );
    }

    #[test]
    fn mismatched_removal_restores_only_when_public_name_is_vacant() {
        let backup = PathBuf::from("/private-stage/removed");
        let restored = restore_mismatched_removal(&backup, "different bytes", || Ok(()));
        assert!(!restored.wrote);
        assert!(restored.residue.is_none());
        assert!(restored.detail.contains("restored"));

        let retained = restore_mismatched_removal(&backup, "different bytes", || {
            Err(io::Error::from(io::ErrorKind::AlreadyExists))
        });
        assert!(retained.wrote);
        assert_eq!(retained.residue, Some(backup));
        assert!(retained.detail.contains("later public arrival"));
    }

    #[test]
    fn directory_guard_failure_accounts_for_later_directories_and_files() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("skill");
        write(&root.join("SKILL.md"), b"old\n");
        let original = observe_directory_manifest(&root).unwrap();
        let expected = manifest(&[
            ("a/one.md", false, b"one\n"),
            ("b/two.md", false, b"two\n"),
            ("SKILL.md", false, b"new\n"),
        ]);
        let mut stage = Stage::prepare(&root, &temporary.path().join(".stage"), &expected).unwrap();
        let mut calls = 0;
        let report = stage.apply_with_guard(&original, || {
            calls += 1;
            (calls == 1)
                .then_some(())
                .ok_or_else(|| "source changed".into())
        });
        assert_eq!(report.completed, vec![root.join("a")]);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].path, root.join("b"));
        assert_eq!(
            report.unattempted,
            vec![
                root.join("SKILL.md"),
                root.join("a/one.md"),
                root.join("b/two.md"),
            ]
        );
    }

    #[test]
    fn transition_guard_failure_accounts_for_the_pending_subtree_and_files() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("skill");
        write(&root.join("assets"), b"old asset\n");
        write(&root.join("SKILL.md"), b"old skill\n");
        let original = observe_directory_manifest(&root).unwrap();
        let expected = manifest(&[
            ("assets/icon.md", false, b"new icon\n"),
            ("SKILL.md", false, b"new skill\n"),
        ]);
        let mut stage = Stage::prepare(&root, &temporary.path().join(".stage"), &expected).unwrap();
        let report = stage.apply_with_guard(&original, || Err("source changed".into()));
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].path, root.join("assets"));
        assert_eq!(
            report.unattempted,
            vec![
                root.join("SKILL.md"),
                root.join("assets"),
                root.join("assets/icon.md"),
            ]
        );
    }

    #[test]
    fn completed_transition_removal_still_leaves_its_directory_operation_unattempted() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("skill");
        write(&root.join("assets-one"), b"old one\n");
        write(&root.join("assets-two"), b"old two\n");
        let original = observe_directory_manifest(&root).unwrap();
        let expected = manifest(&[
            ("assets-one/icon.md", false, b"new one\n"),
            ("assets-two/icon.md", false, b"new two\n"),
        ]);
        let mut stage = Stage::prepare(&root, &temporary.path().join(".stage"), &expected).unwrap();
        let mut calls = 0;
        let report = stage.apply_with_guard(&original, || {
            calls += 1;
            (calls == 1)
                .then_some(())
                .ok_or_else(|| "source changed".into())
        });
        assert_eq!(report.completed, vec![root.join("assets-one")]);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].path, root.join("assets-two"));
        assert_eq!(
            report.unattempted,
            vec![
                root.join("assets-one"),
                root.join("assets-one/icon.md"),
                root.join("assets-two"),
                root.join("assets-two/icon.md"),
            ]
        );
    }

    #[test]
    fn post_mkdir_failure_is_partial_and_accounts_for_pending_file_tail() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("skill");
        fs::create_dir(&root).unwrap();
        let original = observe_directory_manifest(&root).unwrap();
        let expected = manifest(&[("new-directory/file.md", false, b"new\n")]);
        let stage = Stage::prepare(&root, &temporary.path().join(".stage"), &expected).unwrap();
        let error = stage
            .create_directory_with(Path::new("new-directory"), |_parent, _name| {
                Err("injected post-mkdir verification failure".into())
            })
            .unwrap_err();
        assert!(root.join("new-directory").is_dir());
        let mut report = ReplacementReport::default();
        record_file_error(&mut report, root.join("new-directory"), error);
        let report = stage.finalize_report(report, &original);
        assert!(report.changed());
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].path, root.join("new-directory"));
        assert_eq!(report.unattempted, vec![root.join("new-directory/file.md")]);
    }
}
