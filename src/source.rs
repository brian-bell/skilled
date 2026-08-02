use std::{
    ffi::{OsStr, OsString},
    fs,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
};

use crate::{
    Error, Result,
    agents::AgentKind,
    validation::{PortableValidationError, validate_portable_skill},
};

const MAX_SCAN_DEPTH: usize = 12;
const MAX_SCANNED_ENTRIES: usize = 4_096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogClassification {
    Common,
    AgentSpecific,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Compatibility {
    claude_code: bool,
    codex: bool,
    opencode: bool,
}

impl Compatibility {
    const ALL: Self = Self {
        claude_code: true,
        codex: true,
        opencode: true,
    };

    const CLAUDE_CODE: Self = Self {
        claude_code: true,
        codex: false,
        opencode: false,
    };

    const CODEX: Self = Self {
        claude_code: false,
        codex: true,
        opencode: false,
    };

    const OPENCODE: Self = Self {
        claude_code: false,
        codex: false,
        opencode: true,
    };

    pub fn claude_code(self) -> bool {
        self.claude_code
    }

    pub fn codex(self) -> bool {
        self.codex
    }

    pub fn opencode(self) -> bool {
        self.opencode
    }

    pub fn all_supported(self) -> bool {
        self == Self::ALL
    }

    pub(crate) fn from_flags(claude_code: bool, codex: bool, opencode: bool) -> Self {
        Self {
            claude_code,
            codex,
            opencode,
        }
    }

    fn toggle(&mut self, agent: AgentKind) {
        match agent {
            AgentKind::ClaudeCode => self.claude_code = !self.claude_code,
            AgentKind::Codex => self.codex = !self.codex,
            AgentKind::OpenCode => self.opencode = !self.opencode,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillCandidate {
    directory_name: String,
    relative_path: PathBuf,
    validation: SkillValidation,
    content_fingerprint: Option<u64>,
    has_exact_skill_md: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SkillValidation {
    Valid { name: String, description: String },
    Invalid { message: String },
}

impl SkillValidation {
    pub fn is_valid(&self) -> bool {
        matches!(self, Self::Valid { .. })
    }

    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Valid { .. } => None,
            Self::Invalid { message } => Some(message),
        }
    }
}

impl SkillCandidate {
    pub fn directory_name(&self) -> &str {
        &self.directory_name
    }

    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    pub fn validation(&self) -> &SkillValidation {
        &self.validation
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogProposal {
    relative_path: PathBuf,
    classification: CatalogClassification,
    compatibility: Compatibility,
    candidates: Vec<SkillCandidate>,
    included: bool,
    scan_error: Option<String>,
}

impl CatalogProposal {
    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    pub fn classification(&self) -> CatalogClassification {
        self.classification
    }

    pub fn compatibility(&self) -> Compatibility {
        self.compatibility
    }

    pub fn candidates(&self) -> &[SkillCandidate] {
        &self.candidates
    }

    pub fn included(&self) -> bool {
        self.included
    }

    pub fn scan_error(&self) -> Option<&str> {
        self.scan_error.as_deref()
    }

    pub(crate) fn toggle_included(&mut self) {
        self.included = !self.included;
    }

    pub(crate) fn toggle_classification(&mut self) {
        self.classification = match self.classification {
            CatalogClassification::Common => CatalogClassification::AgentSpecific,
            CatalogClassification::AgentSpecific => CatalogClassification::Common,
        };
    }

    pub(crate) fn toggle_compatibility(&mut self, agent: AgentKind) {
        self.compatibility.toggle(agent);
    }

    pub(crate) fn from_confirmed(
        git_top_level: &Path,
        relative_path: PathBuf,
        classification: CatalogClassification,
        compatibility: Compatibility,
    ) -> Self {
        let (candidates, scan_error) =
            match confirmed_catalog_candidates(git_top_level, &relative_path) {
                Ok(candidates) => (candidates, None),
                Err(error) => (Vec::new(), Some(error.to_string())),
            };
        Self {
            relative_path,
            classification,
            compatibility,
            candidates,
            included: true,
            scan_error,
        }
    }

    pub(crate) fn from_unavailable(
        relative_path: PathBuf,
        classification: CatalogClassification,
        compatibility: Compatibility,
        error: &str,
    ) -> Self {
        Self {
            relative_path,
            classification,
            compatibility,
            candidates: Vec::new(),
            included: true,
            scan_error: Some(error.to_owned()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourcePreview {
    inspected: InspectedSource,
    catalogs: Vec<CatalogProposal>,
}

impl SourcePreview {
    pub fn inspected(&self) -> &InspectedSource {
        &self.inspected
    }

    pub fn catalogs(&self) -> &[CatalogProposal] {
        &self.catalogs
    }

    pub(crate) fn catalog_mut(&mut self, index: usize) -> Option<&mut CatalogProposal> {
        self.catalogs.get_mut(index)
    }

    pub(crate) fn has_included_catalog(&self) -> bool {
        self.catalogs.iter().any(CatalogProposal::included)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredSource {
    id: i64,
    label: String,
    inspected: InspectedSource,
    catalogs: Vec<CatalogProposal>,
    last_scan_at: i64,
    source_error: Option<String>,
}

impl RegisteredSource {
    pub fn id(&self) -> i64 {
        self.id
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn git_top_level(&self) -> &Path {
        self.inspected.git_top_level()
    }

    pub fn branch(&self) -> Option<&str> {
        self.inspected.branch()
    }

    pub fn head(&self) -> &str {
        self.inspected.head()
    }

    pub fn remote_url(&self) -> Option<&str> {
        self.inspected.remote_url()
    }

    pub fn dirty(&self) -> bool {
        self.inspected.dirty()
    }

    pub fn catalogs(&self) -> &[CatalogProposal] {
        &self.catalogs
    }

    pub fn last_scan_at(&self) -> i64 {
        self.last_scan_at
    }

    pub fn source_error(&self) -> Option<&str> {
        self.source_error.as_deref()
    }

    pub(crate) fn new(
        id: i64,
        label: String,
        inspected: InspectedSource,
        catalogs: Vec<CatalogProposal>,
        last_scan_at: i64,
        source_error: Option<String>,
    ) -> Self {
        Self {
            id,
            label,
            inspected,
            catalogs,
            last_scan_at,
            source_error,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectedSource {
    git_top_level: PathBuf,
    branch: Option<String>,
    head: String,
    remote_url: Option<String>,
    dirty: bool,
}

impl InspectedSource {
    pub fn git_top_level(&self) -> &Path {
        &self.git_top_level
    }

    pub fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }

    pub fn head(&self) -> &str {
        &self.head
    }

    pub fn remote_url(&self) -> Option<&str> {
        self.remote_url.as_deref()
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }
}

pub fn inspect_local_source(path: &Path) -> Result<InspectedSource> {
    let input = path.canonicalize()?;
    if !input.is_dir() {
        return Err(Error::SourcePathNotDirectory(input));
    }

    let top_level = required_git_bytes(&input, &["rev-parse", "--show-toplevel"])?;
    let git_top_level = git_path_from_output(top_level)?.canonicalize()?;
    if !input.starts_with(&git_top_level) {
        return Err(Error::SourceOutsideGitTopLevel {
            path: input,
            top_level: git_top_level,
        });
    }

    let head = required_git_output(&git_top_level, &["rev-parse", "HEAD"])?;
    let branch = optional_git_output(
        &git_top_level,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    )?;
    let remote_url = optional_git_output(&git_top_level, &["remote", "get-url", "origin"])?
        .map(|url| sanitize_remote_url(&url));
    let status = required_git_output(
        &git_top_level,
        &["status", "--porcelain=v1", "--untracked-files=normal"],
    )?;

    Ok(InspectedSource {
        git_top_level,
        branch,
        head: strip_record_terminator(&head).to_owned(),
        remote_url,
        dirty: !status.is_empty(),
    })
}

pub(crate) fn contains_revision(repository: &Path, revision: &str) -> Result<bool> {
    let commit = format!("{revision}^{{commit}}");
    Ok(run_git(repository, &["cat-file", "-e", &commit])?
        .status
        .success())
}

pub fn preview_local_source(path: &Path) -> Result<SourcePreview> {
    let inspected = inspect_local_source(path)?;
    let catalogs = propose_catalogs(inspected.git_top_level())?;
    Ok(SourcePreview {
        inspected,
        catalogs,
    })
}

pub fn revalidate_source_preview(preview: &SourcePreview) -> Result<SourcePreview> {
    let current = preview_local_source(preview.inspected.git_top_level())?;
    if current.inspected != preview.inspected {
        return Err(Error::SourceChangedAfterPreview);
    }

    let mut catalogs = Vec::new();
    for selected in preview.catalogs.iter().filter(|catalog| catalog.included) {
        let Some(current_catalog) = current
            .catalogs
            .iter()
            .find(|catalog| catalog.relative_path == selected.relative_path)
        else {
            return Err(Error::SourceChangedAfterPreview);
        };
        if current_catalog.candidates != selected.candidates {
            return Err(Error::SourceChangedAfterPreview);
        }
        let mut confirmed = current_catalog.clone();
        confirmed.classification = selected.classification;
        confirmed.compatibility = selected.compatibility;
        confirmed.included = true;
        catalogs.push(confirmed);
    }
    if catalogs.is_empty() {
        return Err(Error::NoCatalogsSelected);
    }
    Ok(SourcePreview {
        inspected: current.inspected,
        catalogs,
    })
}

impl InspectedSource {
    pub(crate) fn from_stored(
        git_top_level: PathBuf,
        branch: Option<String>,
        head: String,
        remote_url: Option<String>,
        dirty: bool,
    ) -> Self {
        Self {
            git_top_level,
            branch,
            head,
            remote_url: remote_url.map(|url| sanitize_remote_url(&url)),
            dirty,
        }
    }
}

pub fn propose_catalogs(git_top_level: &Path) -> Result<Vec<CatalogProposal>> {
    let git_top_level = git_top_level.canonicalize()?;
    let mut root_entries = MAX_SCANNED_ENTRIES;
    if exact_skill_md_exists(&git_top_level, &mut root_entries)? {
        let directory_name = path_file_name(&git_top_level)?;
        let (validation, has_exact_skill_md) = validation_for(&git_top_level);
        return Ok(vec![CatalogProposal {
            relative_path: PathBuf::from("."),
            classification: CatalogClassification::Common,
            compatibility: Compatibility::ALL,
            candidates: vec![SkillCandidate {
                directory_name,
                relative_path: PathBuf::from("."),
                validation,
                content_fingerprint: skill_document_fingerprint(&git_top_level),
                has_exact_skill_md,
            }],
            included: true,
            scan_error: None,
        }]);
    }

    let mut proposals = Vec::new();
    let mut pending = vec![(git_top_level.clone(), 0_usize)];
    let mut remaining_entries = MAX_SCANNED_ENTRIES;
    while let Some((directory, depth)) = pending.pop() {
        if depth > MAX_SCAN_DEPTH {
            continue;
        }

        let mut children = child_directories(&directory, &mut remaining_entries)?;
        children.sort();
        children.reverse();
        for child in children {
            if child.file_name() == Some(OsStr::new(".git")) {
                continue;
            }
            if let Some((classification, compatibility)) = catalog_defaults(&git_top_level, &child)
            {
                let candidates =
                    catalog_candidates_with_budget(&git_top_level, &child, &mut remaining_entries)?;
                if candidates
                    .iter()
                    .any(|candidate| candidate.has_exact_skill_md)
                {
                    proposals.push(CatalogProposal {
                        relative_path: child
                            .strip_prefix(&git_top_level)
                            .expect("catalog beneath source root")
                            .to_path_buf(),
                        classification,
                        compatibility,
                        candidates,
                        included: true,
                        scan_error: None,
                    });
                    continue;
                }
            }
            pending.push((child, depth + 1));
        }
    }
    proposals.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(proposals)
}

fn catalog_defaults(
    git_top_level: &Path,
    candidate: &Path,
) -> Option<(CatalogClassification, Compatibility)> {
    if candidate.file_name()? != OsStr::new("skills") {
        return None;
    }
    if candidate.parent() == Some(git_top_level) {
        return Some((CatalogClassification::Common, Compatibility::ALL));
    }
    match candidate.parent()?.file_name()?.to_str()? {
        "claude" | "claude-code" | ".claude" => Some((
            CatalogClassification::AgentSpecific,
            Compatibility::CLAUDE_CODE,
        )),
        "codex" | ".agents" => Some((CatalogClassification::AgentSpecific, Compatibility::CODEX)),
        "opencode" => Some((
            CatalogClassification::AgentSpecific,
            Compatibility::OPENCODE,
        )),
        _ => None,
    }
}

fn catalog_candidates(git_top_level: &Path, catalog: &Path) -> Result<Vec<SkillCandidate>> {
    let mut remaining_entries = MAX_SCANNED_ENTRIES;
    catalog_candidates_with_budget(git_top_level, catalog, &mut remaining_entries)
}

fn catalog_candidates_with_budget(
    git_top_level: &Path,
    catalog: &Path,
    remaining_entries: &mut usize,
) -> Result<Vec<SkillCandidate>> {
    let mut candidates = child_directories(catalog, remaining_entries)?
        .into_iter()
        .map(|path| skill_candidate(git_top_level, &path))
        .collect::<Result<Vec<_>>>()?;
    candidates.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(candidates)
}

fn confirmed_catalog_candidates(
    git_top_level: &Path,
    relative_path: &Path,
) -> Result<Vec<SkillCandidate>> {
    if relative_path.as_os_str().is_empty()
        || relative_path.is_absolute()
        || relative_path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(Error::UnsafeCatalogPath(relative_path.to_path_buf()));
    }

    let canonical_source = git_top_level.canonicalize()?;
    if relative_path == Path::new(".") {
        let (validation, has_exact_skill_md) = validation_for(&canonical_source);
        return Ok(vec![SkillCandidate {
            directory_name: canonical_source
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| ".".to_owned()),
            relative_path: PathBuf::from("."),
            validation,
            content_fingerprint: skill_document_fingerprint(&canonical_source),
            has_exact_skill_md,
        }]);
    }

    let catalog = canonical_source.join(relative_path);
    if fs::symlink_metadata(&catalog)?.file_type().is_symlink() {
        return Err(Error::CatalogRootSymlink(catalog));
    }
    let canonical_catalog = catalog.canonicalize()?;
    if !canonical_catalog.starts_with(&canonical_source) {
        return Err(Error::CatalogOutsideSource(canonical_catalog));
    }
    catalog_candidates(&canonical_source, &canonical_catalog)
}

fn skill_candidate(git_top_level: &Path, path: &Path) -> Result<SkillCandidate> {
    let (validation, has_exact_skill_md) = validation_for(path);
    Ok(SkillCandidate {
        directory_name: path_file_name(path)?,
        relative_path: path
            .strip_prefix(git_top_level)
            .expect("candidate beneath source root")
            .to_path_buf(),
        validation,
        content_fingerprint: skill_document_fingerprint(path),
        has_exact_skill_md,
    })
}

fn validation_for(path: &Path) -> (SkillValidation, bool) {
    match validate_portable_skill(path) {
        Ok(validated) => (
            SkillValidation::Valid {
                name: validated.name().to_owned(),
                description: validated.description().to_owned(),
            },
            true,
        ),
        Err(error) => {
            let has_exact_skill_md = !matches!(
                error,
                PortableValidationError::MissingSkillMd
                    | PortableValidationError::ReadDirectory { .. }
            );
            (
                SkillValidation::Invalid {
                    message: error.to_string(),
                },
                has_exact_skill_md,
            )
        }
    }
}

fn child_directories(directory: &Path, remaining_entries: &mut usize) -> Result<Vec<PathBuf>> {
    let mut directories = Vec::new();
    for entry in fs::read_dir(directory)? {
        if *remaining_entries == 0 {
            return Err(Error::SourceScanLimitExceeded);
        }
        *remaining_entries -= 1;
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() && !file_type.is_symlink() {
            directories.push(entry.path());
        }
    }
    Ok(directories)
}

fn exact_skill_md_exists(directory: &Path, remaining_entries: &mut usize) -> Result<bool> {
    for entry in fs::read_dir(directory)? {
        if *remaining_entries == 0 {
            return Err(Error::SourceScanLimitExceeded);
        }
        *remaining_entries -= 1;
        let entry = entry?;
        if entry.file_name() == OsStr::new("SKILL.md") {
            return Ok(entry.file_type()?.is_file());
        }
    }
    Ok(false)
}

fn skill_document_fingerprint(directory: &Path) -> Option<u64> {
    use std::io::Read;

    const MAX_FINGERPRINT_BYTES: u64 = 1024 * 1024 + 1;
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let path = directory.join("SKILL.md");
    let metadata = fs::symlink_metadata(&path).ok()?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return None;
    }
    let canonical_directory = directory.canonicalize().ok()?;
    let canonical_path = path.canonicalize().ok()?;
    if !canonical_path.starts_with(canonical_directory) {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(canonical_path)
        .ok()?
        .take(MAX_FINGERPRINT_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes.into_iter().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
    }))
}

fn sanitize_remote_url(value: &str) -> String {
    let value = value.split_once(['?', '#']).map_or(value, |(base, _)| base);
    if let Some((scheme, remainder)) = value.split_once("://") {
        let authority_end = remainder.find('/').unwrap_or(remainder.len());
        let (authority, path) = remainder.split_at(authority_end);
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        return format!("{scheme}://{host}{path}");
    }
    if let Some((_, remote)) = value.rsplit_once('@') {
        return remote.to_owned();
    }
    value.to_owned()
}

fn path_file_name(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(OsStr::to_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::InvalidSourcePath(path.to_path_buf()))
}

fn required_git_output(repository: &Path, arguments: &[&str]) -> Result<String> {
    String::from_utf8(required_git_bytes(repository, arguments)?)
        .map_err(|_| Error::InvalidGitOutput)
}

fn required_git_bytes(repository: &Path, arguments: &[&str]) -> Result<Vec<u8>> {
    let output = run_git(repository, arguments)?;
    if !output.status.success() {
        return Err(git_error(repository, arguments, &output));
    }
    Ok(output.stdout)
}

fn optional_git_output(repository: &Path, arguments: &[&str]) -> Result<Option<String>> {
    let output = run_git(repository, arguments)?;
    if !output.status.success() {
        return Ok(None);
    }
    let value = String::from_utf8(output.stdout).map_err(|_| Error::InvalidGitOutput)?;
    let value = strip_record_terminator(&value);
    Ok((!value.is_empty()).then(|| value.to_owned()))
}

fn run_git(repository: &Path, arguments: &[&str]) -> Result<Output> {
    Command::new("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
        ])
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .map_err(Error::GitUnavailable)
}

fn strip_record_terminator(value: &str) -> &str {
    value
        .strip_suffix("\r\n")
        .or_else(|| value.strip_suffix('\n'))
        .unwrap_or(value)
}

#[cfg(unix)]
fn git_path_from_output(mut value: Vec<u8>) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;

    if value.ends_with(b"\r\n") {
        value.truncate(value.len() - 2);
    } else if value.ends_with(b"\n") {
        value.pop();
    }
    Ok(PathBuf::from(OsString::from_vec(value)))
}

#[cfg(not(unix))]
fn git_path_from_output(value: Vec<u8>) -> Result<PathBuf> {
    let value = String::from_utf8(value).map_err(|_| Error::InvalidGitOutput)?;
    Ok(PathBuf::from(strip_record_terminator(&value)))
}

fn git_error(repository: &Path, arguments: &[&str], output: &Output) -> Error {
    Error::GitCommand {
        repository: repository.to_path_buf(),
        arguments: arguments.iter().map(|value| (*value).to_owned()).collect(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    }
}
