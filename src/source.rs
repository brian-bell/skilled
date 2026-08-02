use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use crate::{Error, Result, agents::AgentKind, validation::validate_portable_skill};

const MAX_SCAN_DEPTH: usize = 12;
const MAX_SCANNED_DIRECTORIES: usize = 50_000;

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
    ) -> Result<Self> {
        let candidates = if relative_path == Path::new(".") {
            vec![SkillCandidate {
                directory_name: path_file_name(git_top_level)?,
                relative_path: PathBuf::from("."),
                validation: validation_for(git_top_level),
            }]
        } else {
            catalog_candidates(git_top_level, &git_top_level.join(&relative_path))?
        };
        Ok(Self {
            relative_path,
            classification,
            compatibility,
            candidates,
            included: true,
        })
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

    pub(crate) fn new(
        id: i64,
        label: String,
        inspected: InspectedSource,
        catalogs: Vec<CatalogProposal>,
        last_scan_at: i64,
    ) -> Self {
        Self {
            id,
            label,
            inspected,
            catalogs,
            last_scan_at,
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

    let top_level = required_git_output(&input, &["rev-parse", "--show-toplevel"])?;
    let git_top_level = PathBuf::from(top_level.trim()).canonicalize()?;
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
    let remote_url = optional_git_output(&git_top_level, &["remote", "get-url", "origin"])?;
    let status = required_git_output(
        &git_top_level,
        &["status", "--porcelain=v1", "--untracked-files=normal"],
    )?;

    Ok(InspectedSource {
        git_top_level,
        branch,
        head: head.trim().to_owned(),
        remote_url,
        dirty: !status.is_empty(),
    })
}

pub fn preview_local_source(path: &Path) -> Result<SourcePreview> {
    let inspected = inspect_local_source(path)?;
    let catalogs = propose_catalogs(inspected.git_top_level())?;
    Ok(SourcePreview {
        inspected,
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
            remote_url,
            dirty,
        }
    }
}

pub fn propose_catalogs(git_top_level: &Path) -> Result<Vec<CatalogProposal>> {
    let git_top_level = git_top_level.canonicalize()?;
    if exact_skill_md_exists(&git_top_level)? {
        let directory_name = path_file_name(&git_top_level)?;
        return Ok(vec![CatalogProposal {
            relative_path: PathBuf::from("."),
            classification: CatalogClassification::Common,
            compatibility: Compatibility::ALL,
            candidates: vec![SkillCandidate {
                directory_name,
                relative_path: PathBuf::from("."),
                validation: validation_for(&git_top_level),
            }],
            included: true,
        }]);
    }

    let mut proposals = Vec::new();
    let mut pending = vec![(git_top_level.clone(), 0_usize)];
    let mut scanned_directories = 0_usize;
    while let Some((directory, depth)) = pending.pop() {
        scanned_directories += 1;
        if scanned_directories > MAX_SCANNED_DIRECTORIES {
            return Err(Error::SourceScanLimitExceeded);
        }
        if depth > MAX_SCAN_DEPTH {
            continue;
        }

        let mut children = child_directories(&directory)?;
        children.sort();
        children.reverse();
        for child in children {
            if child.file_name() == Some(OsStr::new(".git")) {
                continue;
            }
            if let Some((classification, compatibility)) = catalog_defaults(&git_top_level, &child)
            {
                let candidates = catalog_candidates(&git_top_level, &child)?;
                let mut has_exact_skill_md = false;
                for candidate in &candidates {
                    has_exact_skill_md |=
                        exact_skill_md_exists(&git_top_level.join(candidate.relative_path()))?;
                }
                if has_exact_skill_md {
                    proposals.push(CatalogProposal {
                        relative_path: child
                            .strip_prefix(&git_top_level)
                            .expect("catalog beneath source root")
                            .to_path_buf(),
                        classification,
                        compatibility,
                        candidates,
                        included: true,
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
    let mut candidates = child_directories(catalog)?
        .into_iter()
        .map(|path| skill_candidate(git_top_level, &path))
        .collect::<Result<Vec<_>>>()?;
    candidates.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(candidates)
}

fn skill_candidate(git_top_level: &Path, path: &Path) -> Result<SkillCandidate> {
    Ok(SkillCandidate {
        directory_name: path_file_name(path)?,
        relative_path: path
            .strip_prefix(git_top_level)
            .expect("candidate beneath source root")
            .to_path_buf(),
        validation: validation_for(path),
    })
}

fn validation_for(path: &Path) -> SkillValidation {
    match validate_portable_skill(path) {
        Ok(validated) => SkillValidation::Valid {
            name: validated.name().to_owned(),
            description: validated.description().to_owned(),
        },
        Err(error) => SkillValidation::Invalid {
            message: error.to_string(),
        },
    }
}

fn child_directories(directory: &Path) -> Result<Vec<PathBuf>> {
    fs::read_dir(directory)?
        .filter_map(|entry| match entry {
            Ok(entry) => match entry.file_type() {
                Ok(file_type) if file_type.is_dir() && !file_type.is_symlink() => {
                    Some(Ok(entry.path()))
                }
                Ok(_) => None,
                Err(error) => Some(Err(error.into())),
            },
            Err(error) => Some(Err(error.into())),
        })
        .collect()
}

fn exact_skill_md_exists(directory: &Path) -> Result<bool> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_name() == OsStr::new("SKILL.md") {
            return Ok(entry.metadata()?.is_file());
        }
    }
    Ok(false)
}

fn path_file_name(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(OsStr::to_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::InvalidSourcePath(path.to_path_buf()))
}

fn required_git_output(repository: &Path, arguments: &[&str]) -> Result<String> {
    let output = run_git(repository, arguments)?;
    if !output.status.success() {
        return Err(git_error(repository, arguments, &output));
    }
    String::from_utf8(output.stdout).map_err(|_| Error::InvalidGitOutput)
}

fn optional_git_output(repository: &Path, arguments: &[&str]) -> Result<Option<String>> {
    let output = run_git(repository, arguments)?;
    if !output.status.success() {
        return Ok(None);
    }
    let value = String::from_utf8(output.stdout).map_err(|_| Error::InvalidGitOutput)?;
    let value = value.trim();
    Ok((!value.is_empty()).then(|| value.to_owned()))
}

fn run_git(repository: &Path, arguments: &[&str]) -> Result<Output> {
    Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .map_err(Error::GitUnavailable)
}

fn git_error(repository: &Path, arguments: &[&str], output: &Output) -> Error {
    Error::GitCommand {
        repository: repository.to_path_buf(),
        arguments: arguments.iter().map(|value| (*value).to_owned()).collect(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    }
}
