use std::{
    collections::HashSet,
    env,
    ffi::{OsStr, OsString},
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
};

use crate::{
    Error, Result,
    agents::{AgentKind, adapter},
    validation::{InspectionBudget, PortableValidationError, validate_portable_skill_with_budget},
};

const MAX_SCAN_DEPTH: usize = 12;
const MAX_CATALOG_CANDIDATES: usize = 4_096;
const MAX_FILTERED_STATUS_PATHS: usize = 4_096;
const MAX_FILTERED_STATUS_PATH_BYTES: usize = 4 * 1024 * 1024;

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

    const NONE: Self = Self {
        claude_code: false,
        codex: false,
        opencode: false,
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

    /// Whether this compatibility covers one agent.
    ///
    /// A record, not an observation: it reports what was proposed, confirmed
    /// and stored, and no agent was asked.
    pub fn supports(self, agent: AgentKind) -> bool {
        match agent {
            AgentKind::ClaudeCode => self.claude_code,
            AgentKind::Codex => self.codex,
            AgentKind::OpenCode => self.opencode,
        }
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
    /// A candidate assembled without a scan, for tests that need a catalog in
    /// a state the scanner does not currently produce.
    #[cfg(test)]
    pub(crate) fn for_test(directory_name: &str) -> Self {
        Self {
            directory_name: directory_name.to_owned(),
            relative_path: PathBuf::from(directory_name),
            validation: SkillValidation::Valid {
                name: directory_name.to_owned(),
                description: format!("{directory_name} fixture"),
            },
            content_fingerprint: None,
        }
    }

    /// The same, for a test that needs the candidate to sit in a named catalog.
    ///
    /// A scanned candidate's relative path runs from the checkout root, so a
    /// variant's identity carries its catalog; a fixture built without one
    /// would let two variants of different catalogs share an identity they
    /// never share in practice.
    #[cfg(test)]
    pub(crate) fn for_test_in(catalog_relative_path: &str, directory_name: &str) -> Self {
        Self {
            relative_path: PathBuf::from(catalog_relative_path).join(directory_name),
            ..Self::for_test(directory_name)
        }
    }

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
    /// A catalog with the candidates and scan error a test asks for, and
    /// settled defaults for everything else.
    ///
    /// The scanning constructors below empty the candidates whenever they
    /// record an error, so a catalog holding both is unreachable through
    /// them. This exists for the tests that need to prove the rest of the
    /// application does not depend on that coincidence.
    #[cfg(test)]
    pub(crate) fn for_test(
        relative_path: &str,
        candidates: Vec<SkillCandidate>,
        scan_error: Option<&str>,
    ) -> Self {
        Self {
            relative_path: PathBuf::from(relative_path),
            classification: CatalogClassification::Common,
            compatibility: Compatibility::ALL,
            candidates,
            included: true,
            scan_error: scan_error.map(str::to_owned),
        }
    }

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
        budget: &mut InspectionBudget,
    ) -> Self {
        let (candidates, scan_error) =
            match confirmed_catalog_candidates(git_top_level, &relative_path, budget) {
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

    /// The recorded revision in the abbreviated form Git itself prints.
    ///
    /// A row that names a repository has room for a revision the reader can
    /// recognise, not for forty characters of one. Seven is Git's own default
    /// abbreviation, and a shorter revision is given whole rather than padded:
    /// this reports what was stored, it does not resolve anything.
    pub fn short_head(&self) -> &str {
        const SHORT_HEAD_LENGTH: usize = 7;

        let head = self.head();
        // By character, not by byte: a stored revision that is not the hex Git
        // produces must still cut on a boundary rather than panicking.
        head.char_indices()
            .nth(SHORT_HEAD_LENGTH)
            .map_or(head, |(index, _)| &head[..index])
    }

    pub fn remote_url(&self) -> Option<&str> {
        self.inspected.remote_url()
    }

    pub fn dirty(&self) -> Option<bool> {
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
    dirty: Option<bool>,
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

    pub fn dirty(&self) -> Option<bool> {
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
    let dirty = git_status_dirty(&git_top_level)?;

    Ok(InspectedSource {
        git_top_level,
        branch,
        head: strip_record_terminator(&head).to_owned(),
        remote_url,
        dirty,
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
        dirty: Option<bool>,
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
    let mut budget = InspectionBudget::source_scan();
    if exact_skill_md_exists(&git_top_level, &mut budget)? {
        let directory_name = path_file_name(&git_top_level)?;
        let validation = validation_for(&git_top_level, &mut budget)?;
        return Ok(vec![CatalogProposal {
            relative_path: PathBuf::from("."),
            classification: CatalogClassification::Common,
            compatibility: Compatibility::ALL,
            candidates: vec![SkillCandidate {
                directory_name,
                relative_path: PathBuf::from("."),
                validation,
                content_fingerprint: skill_document_fingerprint(&git_top_level, &mut budget)?,
            }],
            included: true,
            scan_error: None,
        }]);
    }

    let mut proposals = Vec::new();
    let mut pending = vec![(git_top_level.clone(), 0_usize)];
    while let Some((directory, depth)) = pending.pop() {
        if depth > MAX_SCAN_DEPTH {
            continue;
        }

        let mut children = child_directories(&directory, &mut budget, None)?;
        children.sort();
        children.reverse();
        for child in children {
            if is_ignored_scan_directory(&child) {
                continue;
            }
            if let Some((classification, compatibility)) = catalog_defaults(&git_top_level, &child)
            {
                let candidates =
                    catalog_candidates_with_budget(&git_top_level, &child, &mut budget)?;
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
            pending.push((child, depth + 1));
        }
    }
    proposals.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(proposals)
}

fn is_ignored_scan_directory(directory: &Path) -> bool {
    matches!(
        directory.file_name().and_then(OsStr::to_str),
        Some(".git" | "node_modules" | "target")
    )
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
    let relative_path = candidate.strip_prefix(git_top_level).ok()?;
    let mut compatibility = Compatibility::NONE;
    for kind in AgentKind::ALL {
        if adapter(kind).supports_source_catalog(relative_path) {
            compatibility.toggle(kind);
        }
    }
    (compatibility != Compatibility::NONE)
        .then_some((CatalogClassification::AgentSpecific, compatibility))
}

fn catalog_candidates_with_budget(
    git_top_level: &Path,
    catalog: &Path,
    budget: &mut InspectionBudget,
) -> Result<Vec<SkillCandidate>> {
    let mut candidates = child_directories(catalog, budget, Some(MAX_CATALOG_CANDIDATES))?
        .into_iter()
        .map(|path| skill_candidate(git_top_level, &path, budget))
        .collect::<Result<Vec<_>>>()?;
    candidates.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(candidates)
}

fn confirmed_catalog_candidates(
    git_top_level: &Path,
    relative_path: &Path,
    budget: &mut InspectionBudget,
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
        let validation = validation_for(&canonical_source, budget)?;
        return Ok(vec![SkillCandidate {
            directory_name: canonical_source
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| ".".to_owned()),
            relative_path: PathBuf::from("."),
            validation,
            content_fingerprint: skill_document_fingerprint(&canonical_source, budget)?,
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
    catalog_candidates_with_budget(&canonical_source, &canonical_catalog, budget)
}

fn skill_candidate(
    git_top_level: &Path,
    path: &Path,
    budget: &mut InspectionBudget,
) -> Result<SkillCandidate> {
    let validation = validation_for(path, budget)?;
    Ok(SkillCandidate {
        directory_name: path_file_name(path)?,
        relative_path: path
            .strip_prefix(git_top_level)
            .expect("candidate beneath source root")
            .to_path_buf(),
        validation,
        content_fingerprint: skill_document_fingerprint(path, budget)?,
    })
}

fn validation_for(path: &Path, budget: &mut InspectionBudget) -> Result<SkillValidation> {
    match validate_portable_skill_with_budget(path, budget) {
        Ok(validated) => Ok(SkillValidation::Valid {
            name: validated.name().to_owned(),
            description: validated.description().to_owned(),
        }),
        Err(PortableValidationError::SourceInspectionLimitExceeded) => {
            Err(Error::SourceScanLimitExceeded)
        }
        Err(error) => Ok(SkillValidation::Invalid {
            message: error.to_string(),
        }),
    }
}

fn child_directories(
    directory: &Path,
    budget: &mut InspectionBudget,
    max_directories: Option<usize>,
) -> Result<Vec<PathBuf>> {
    let mut directories = Vec::new();
    for entry in fs::read_dir(directory)? {
        if !budget.consume_entry() {
            return Err(Error::SourceScanLimitExceeded);
        }
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() && !file_type.is_symlink() {
            if max_directories.is_some_and(|limit| directories.len() == limit) {
                return Err(Error::SourceScanLimitExceeded);
            }
            directories.push(entry.path());
        }
    }
    Ok(directories)
}

fn exact_skill_md_exists(directory: &Path, budget: &mut InspectionBudget) -> Result<bool> {
    for entry in fs::read_dir(directory)? {
        if !budget.consume_entry() {
            return Err(Error::SourceScanLimitExceeded);
        }
        let entry = entry?;
        if entry.file_name() == OsStr::new("SKILL.md") {
            return Ok(entry.file_type()?.is_file());
        }
    }
    Ok(false)
}

fn skill_document_fingerprint(
    directory: &Path,
    budget: &mut InspectionBudget,
) -> Result<Option<u64>> {
    use std::io::Read;

    const MAX_FINGERPRINT_BYTES: u64 = 1024 * 1024 + 1;
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let path = directory.join("SKILL.md");
    let Ok(metadata) = fs::symlink_metadata(&path) else {
        return Ok(None);
    };
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Ok(None);
    }
    let Ok(canonical_directory) = directory.canonicalize() else {
        return Ok(None);
    };
    let Ok(canonical_path) = path.canonicalize() else {
        return Ok(None);
    };
    if !canonical_path.starts_with(canonical_directory) {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    let Ok(file) = fs::File::open(canonical_path) else {
        return Ok(None);
    };
    file.take(MAX_FINGERPRINT_BYTES)
        .read_to_end(&mut bytes)
        .map_err(Error::Io)?;
    if !budget.consume_bytes(bytes.len()) {
        return Err(Error::SourceScanLimitExceeded);
    }
    Ok(Some(bytes.into_iter().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
    })))
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
    if !Path::new(value).is_absolute()
        && !value.starts_with("./")
        && !value.starts_with("../")
        && let Some((_, remote)) = value.rsplit_once('@')
        && let Some((host, path)) = remote.split_once(':')
        && !host.is_empty()
        && !path.is_empty()
        && !host.contains(['/', '\\'])
    {
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

fn git_status_dirty(repository: &Path) -> Result<Option<bool>> {
    let arguments = [
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=normal",
        "--ignore-submodules=dirty",
    ];
    let filter_settings = configured_filter_settings(repository)?;
    let inherited_config_count = env::var("GIT_CONFIG_COUNT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut command = git_command(repository, &arguments);
    command.env(
        "GIT_CONFIG_COUNT",
        inherited_config_count
            .checked_add(filter_settings.len())
            .ok_or(Error::InvalidGitOutput)?
            .to_string(),
    );
    for (offset, (key, value)) in filter_settings.iter().enumerate() {
        let index = inherited_config_count + offset;
        command
            .env(format!("GIT_CONFIG_KEY_{index}"), key)
            .env(format!("GIT_CONFIG_VALUE_{index}"), value);
    }
    let configured_drivers = filter_settings
        .iter()
        .filter_map(|(key, _)| configured_filter_driver(key))
        .collect::<HashSet<_>>();
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(Error::GitUnavailable)?;
    let stdout = child.stdout.take().expect("Git status stdout is piped");
    let mut stdout = BufReader::new(stdout);
    let mut record = Vec::new();
    let mut worktree_modifications = Vec::new();
    let mut worktree_modification_bytes = 0_usize;
    while stdout.read_until(0, &mut record)? != 0 {
        if record.last() == Some(&0) {
            record.pop();
        }
        let Some(path) = worktree_modification_path(&record) else {
            drop(stdout);
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Some(true));
        };
        if configured_drivers.is_empty() {
            drop(stdout);
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Some(true));
        }
        let Some(next_bytes) = worktree_modification_bytes.checked_add(path.len()) else {
            drop(stdout);
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Some(true));
        };
        if worktree_modifications.len() == MAX_FILTERED_STATUS_PATHS
            || next_bytes > MAX_FILTERED_STATUS_PATH_BYTES
        {
            drop(stdout);
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Some(true));
        }
        worktree_modification_bytes = next_bytes;
        worktree_modifications.push(path.to_vec());
        record.clear();
    }
    drop(stdout);
    let status = child.wait()?;
    if !status.success() {
        return Err(Error::GitCommand {
            repository: repository.to_path_buf(),
            arguments: arguments.iter().map(|value| (*value).to_owned()).collect(),
            stderr: "git status failed before producing output".to_owned(),
        });
    }
    if worktree_modifications.is_empty() {
        return Ok(Some(false));
    }
    let drivers = filter_drivers_for_paths(repository, &worktree_modifications)?;
    let all_filter_ambiguous = drivers.iter().all(|driver| {
        driver
            .as_deref()
            .is_some_and(|driver| configured_drivers.contains(driver))
    });
    Ok((!all_filter_ambiguous).then_some(true))
}

fn worktree_modification_path(record: &[u8]) -> Option<&[u8]> {
    (record.len() >= 3 && record[..3] == *b" M ").then_some(&record[3..])
}

fn configured_filter_driver(key: &str) -> Option<&str> {
    let (driver, setting) = key.strip_prefix("filter.")?.rsplit_once('.')?;
    matches!(setting, "clean" | "process").then_some(driver)
}

fn filter_drivers_for_paths(repository: &Path, paths: &[Vec<u8>]) -> Result<Vec<Option<String>>> {
    let arguments = ["check-attr", "--stdin", "-z", "filter"];
    let mut child = git_command(repository, &arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(Error::GitUnavailable)?;
    let mut input = Vec::new();
    for path in paths {
        input.extend_from_slice(path);
        input.push(0);
    }
    let mut stdin = child.stdin.take().expect("Git check-attr stdin is piped");
    let mut stdout = child.stdout.take().expect("Git check-attr stdout is piped");
    let mut stderr = child.stderr.take().expect("Git check-attr stderr is piped");
    let writer = match thread::Builder::new()
        .name("git-check-attr-stdin".to_owned())
        .spawn(move || stdin.write_all(&input))
    {
        Ok(writer) => writer,
        Err(error) => {
            terminate_child(&mut child);
            return Err(error.into());
        }
    };
    let stdout_reader = match thread::Builder::new()
        .name("git-check-attr-stdout".to_owned())
        .spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes)?;
            Ok::<_, io::Error>(bytes)
        }) {
        Ok(reader) => reader,
        Err(error) => {
            terminate_child(&mut child);
            let _ = writer.join();
            return Err(error.into());
        }
    };
    let stderr_reader = match thread::Builder::new()
        .name("git-check-attr-stderr".to_owned())
        .spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes)?;
            Ok::<_, io::Error>(bytes)
        }) {
        Ok(reader) => reader,
        Err(error) => {
            terminate_child(&mut child);
            let _ = writer.join();
            let _ = stdout_reader.join();
            return Err(error.into());
        }
    };
    let status = match child.wait() {
        Ok(status) => status,
        Err(error) => {
            terminate_child(&mut child);
            let _ = writer.join();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(error.into());
        }
    };
    let write_result = join_io_thread(writer, "Git check-attr input writer");
    let stdout_result = join_io_thread(stdout_reader, "Git check-attr stdout reader");
    let stderr_result = join_io_thread(stderr_reader, "Git check-attr stderr reader");
    write_result?;
    let stdout = stdout_result?;
    let stderr = stderr_result?;
    let output = Output {
        status,
        stdout,
        stderr,
    };
    if !output.status.success() {
        return Err(git_error(repository, &arguments, &output));
    }
    let mut fields = output.stdout.split(|byte| *byte == 0);
    let mut drivers = Vec::with_capacity(paths.len());
    for expected_path in paths {
        let (Some(path), Some(attribute), Some(value)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return Err(Error::InvalidGitOutput);
        };
        if path != expected_path || attribute != b"filter" {
            return Err(Error::InvalidGitOutput);
        }
        let value = String::from_utf8(value.to_vec()).map_err(|_| Error::InvalidGitOutput)?;
        drivers.push((!matches!(value.as_str(), "unspecified" | "unset")).then_some(value));
    }
    if fields.any(|field| !field.is_empty()) {
        return Err(Error::InvalidGitOutput);
    }
    Ok(drivers)
}

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn join_io_thread<T>(
    thread: thread::JoinHandle<io::Result<T>>,
    description: &'static str,
) -> io::Result<T> {
    thread
        .join()
        .map_err(|_| io::Error::other(format!("{description} panicked")))?
}

fn configured_filter_settings(repository: &Path) -> Result<Vec<(String, &'static str)>> {
    let arguments = [
        "config",
        "--null",
        "--name-only",
        "--get-regexp",
        r"^filter\..*\.(clean|process|required)$",
    ];
    let output = run_git(repository, &arguments)?;
    if !output.status.success() {
        if output.status.code() == Some(1) && output.stdout.is_empty() {
            return Ok(Vec::new());
        }
        return Err(git_error(repository, &arguments, &output));
    }
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|key| !key.is_empty())
        .map(|key| {
            let key = String::from_utf8(key.to_vec()).map_err(|_| Error::InvalidGitOutput)?;
            let value = if key
                .rsplit_once('.')
                .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case("required"))
            {
                "false"
            } else {
                ""
            };
            Ok((key, value))
        })
        .collect()
}

fn run_git(repository: &Path, arguments: &[&str]) -> Result<Output> {
    git_command(repository, arguments)
        .output()
        .map_err(Error::GitUnavailable)
}

fn git_command(repository: &Path, arguments: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
        ])
        .arg("-C")
        .arg(repository)
        .args(arguments);
    command
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

#[cfg(test)]
mod tests {
    use super::*;

    fn registered(head: &str) -> RegisteredSource {
        RegisteredSource::new(
            1,
            "fixture".to_owned(),
            InspectedSource::from_stored(
                PathBuf::from("/fixture"),
                Some("main".to_owned()),
                head.to_owned(),
                None,
                None,
            ),
            Vec::new(),
            0,
            None,
        )
    }

    #[test]
    fn a_short_head_is_the_first_seven_characters_of_the_revision() {
        assert_eq!(
            registered("0123456789abcdef0123456789abcdef01234567").short_head(),
            "0123456"
        );
    }

    #[test]
    fn a_revision_no_longer_than_the_short_form_is_given_whole() {
        assert_eq!(registered("0123456").short_head(), "0123456");
        assert_eq!(registered("abc").short_head(), "abc");
        assert_eq!(registered("").short_head(), "");
    }

    /// Stored revisions come from Git, but the column is bounded by characters
    /// rather than bytes so a corrupted or hand-edited row cannot panic the
    /// renderer on a multi-byte boundary.
    #[test]
    fn a_multi_byte_revision_is_cut_on_a_character_boundary() {
        assert_eq!(registered("äöüäöüäöü").short_head(), "äöüäöüä");
    }
}
