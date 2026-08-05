//! Read-only inventory of the documented native agent skill roots.
//!
//! The scanner observes the immediate children of each selected agent's native
//! global root and classifies what it finds. It never recurses, never launches
//! a coding agent, never spawns any process at all, and never writes. An
//! installation is only claimed as managed when a symbolic link resolves to a
//! skill variant of a registered source; everything else structurally valid
//! stays explicitly unmanaged rather than being adopted.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use crate::{
    AgentDetection, AgentKind,
    source::{RegisteredSource, SkillValidation},
    validation::{InspectionBudget, PortableValidationError, validate_portable_skill_with_budget},
};

/// The largest number of immediate children Skilled will read from one root.
///
/// A root past this size is reported unreadable rather than scanned in part,
/// because a truncated listing would understate what is installed.
pub const MAX_ROOT_CHILDREN: usize = 4_096;

/// What kind of filesystem object occupies an installation slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstallationObject {
    /// A symbolic link, carrying the target exactly as it was recorded.
    Symlink { target: PathBuf },
    /// A physical directory.
    Directory,
    /// Anything else: a regular file, a socket, a device node.
    File,
}

impl InstallationObject {
    pub fn description(&self) -> &'static str {
        match self {
            Self::Symlink { .. } => "symbolic link",
            Self::Directory => "directory",
            Self::File => "file",
        }
    }
}

/// How much attention a finding demands.
///
/// Skilled never repairs anything in this release, so severity orders the
/// display and nothing else.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum FindingSeverity {
    Info,
    Warning,
    Critical,
}

impl FindingSeverity {
    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

/// One observed fact about an installation.
///
/// The code is stable so later slices can act on it; the evidence is the
/// human-readable observation behind it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Finding {
    code: &'static str,
    severity: FindingSeverity,
    evidence: String,
}

impl Finding {
    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn severity(&self) -> FindingSeverity {
        self.severity
    }

    pub fn evidence(&self) -> &str {
        &self.evidence
    }
}

/// The registered source variant an installation was resolved to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VariantResolution {
    source_id: i64,
    source_label: String,
    catalog_relative_path: PathBuf,
    variant_relative_path: PathBuf,
}

impl VariantResolution {
    pub fn source_id(&self) -> i64 {
        self.source_id
    }

    pub fn source_label(&self) -> &str {
        &self.source_label
    }

    pub fn catalog_relative_path(&self) -> &Path {
        &self.catalog_relative_path
    }

    pub fn variant_relative_path(&self) -> &Path {
        &self.variant_relative_path
    }
}

/// The state of an installation, worst-first.
///
/// The ordering is the roll-up rule: a row takes the worst state of the agents
/// that carry it.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum InstallationHealth {
    /// A registered variant, installed as a link, that validates.
    Healthy,
    /// Structurally sound content Skilled does not own.
    Unmanaged,
    /// Content an agent cannot load.
    Broken,
}

impl InstallationHealth {
    pub fn label(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Unmanaged => "unmanaged",
            Self::Broken => "broken",
        }
    }
}

/// One installation slot in one agent's root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledSkillObservation {
    agent: AgentKind,
    name: String,
    path: PathBuf,
    object: InstallationObject,
    resolution: Option<VariantResolution>,
    validation: Option<SkillValidation>,
    findings: Vec<Finding>,
    health: InstallationHealth,
}

impl InstalledSkillObservation {
    pub fn agent(&self) -> AgentKind {
        self.agent
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn object(&self) -> &InstallationObject {
        &self.object
    }

    pub fn resolution(&self) -> Option<&VariantResolution> {
        self.resolution.as_ref()
    }

    /// The portable validation of the installed content, or `None` when the
    /// object could not be treated as a skill directory at all.
    pub fn validation(&self) -> Option<&SkillValidation> {
        self.validation.as_ref()
    }

    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    pub fn health(&self) -> InstallationHealth {
        self.health
    }
}

/// One skill name, across every agent that carries it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InventoryRow {
    name: String,
    observations: [Option<InstalledSkillObservation>; 3],
    health: InstallationHealth,
}

impl InventoryRow {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn observation(&self, agent: AgentKind) -> Option<&InstalledSkillObservation> {
        self.observations[agent.index()].as_ref()
    }

    pub fn observations(&self) -> impl Iterator<Item = &InstalledSkillObservation> {
        self.observations.iter().flatten()
    }

    pub fn health(&self) -> InstallationHealth {
        self.health
    }

    /// The registered source this row was resolved to, if any.
    ///
    /// Agents are consulted in their declared order, so the answer is stable.
    /// Disagreement between agents is a conflict a later slice reports.
    pub fn source_label(&self) -> Option<&str> {
        self.observations()
            .find_map(|observation| observation.resolution())
            .map(VariantResolution::source_label)
    }

    pub fn findings(&self) -> impl Iterator<Item = &Finding> {
        self.observations()
            .flat_map(|observation| observation.findings())
    }
}

/// What Skilled was able to observe about one agent's native root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RootStatus {
    /// The agent is not configured, so its root was left alone.
    NotSelected,
    /// The root does not exist. Absence is not a finding.
    Missing,
    /// The root was read in full.
    Scanned { installed: usize },
    /// The root exists but could not be read in full, so nothing from it is
    /// reported rather than reporting part of it.
    Unreadable { message: String },
}

impl RootStatus {
    /// A phrase naming the state, for a line that has room for one root.
    ///
    /// "root not found" matches the wording agent detection already uses for
    /// the same fact during setup.
    pub fn summary(&self) -> String {
        match self {
            Self::Missing => "root not found".to_owned(),
            Self::Unreadable { .. } => "root unreadable".to_owned(),
            other => other.short_summary(),
        }
    }

    /// The same state, for a line that has to hold all three roots at once.
    pub fn short_summary(&self) -> String {
        match self {
            Self::NotSelected => "not selected".to_owned(),
            Self::Missing => "no root".to_owned(),
            Self::Scanned { installed: 1 } => "1 installed".to_owned(),
            Self::Scanned { installed } => format!("{installed} installed"),
            Self::Unreadable { .. } => "unreadable".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootScan {
    agent: AgentKind,
    path: PathBuf,
    status: RootStatus,
}

impl RootScan {
    pub fn agent(&self) -> AgentKind {
        self.agent
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn status(&self) -> &RootStatus {
        &self.status
    }
}

/// Everything one read-only pass over the native roots observed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InventorySnapshot {
    rows: Vec<InventoryRow>,
    roots: [RootScan; 3],
}

impl InventorySnapshot {
    pub fn rows(&self) -> &[InventoryRow] {
        &self.rows
    }

    pub fn roots(&self) -> &[RootScan; 3] {
        &self.roots
    }

    pub fn root(&self, agent: AgentKind) -> &RootScan {
        &self.roots[agent.index()]
    }

    pub fn row(&self, name: &str) -> Option<&InventoryRow> {
        self.rows.iter().find(|row| row.name() == name)
    }

    /// Installation slots observed, counting each agent separately.
    pub fn installation_count(&self) -> usize {
        self.rows
            .iter()
            .flat_map(InventoryRow::observations)
            .count()
    }

    pub fn unmanaged_count(&self) -> usize {
        self.rows
            .iter()
            .flat_map(InventoryRow::observations)
            .filter(|observation| observation.health() == InstallationHealth::Unmanaged)
            .count()
    }

    pub fn finding_count(&self) -> usize {
        self.rows.iter().flat_map(InventoryRow::findings).count()
    }

    pub fn critical_finding_count(&self) -> usize {
        self.rows
            .iter()
            .flat_map(InventoryRow::findings)
            .filter(|finding| finding.severity() == FindingSeverity::Critical)
            .count()
    }

    /// The snapshot of a session that has not looked at any root yet.
    pub(crate) fn unscanned(agents: &[AgentDetection; 3]) -> Self {
        Self {
            rows: Vec::new(),
            roots: agents.each_ref().map(|agent| RootScan {
                agent: agent.kind(),
                path: agent.root().to_path_buf(),
                status: RootStatus::NotSelected,
            }),
        }
    }
}

/// Observe every selected agent's native root once.
pub(crate) fn scan_installations(
    agents: &[AgentDetection; 3],
    sources: &[RegisteredSource],
) -> InventorySnapshot {
    let index = ResolutionIndex::of(sources);
    let mut budget = InspectionBudget::installation_scan();
    let mut observations = Vec::new();
    let roots = agents.each_ref().map(|agent| {
        let status = if agent.selected() {
            match scan_root(agent, &index, &mut budget) {
                Ok(found) => {
                    let installed = found.len();
                    observations.extend(found);
                    RootStatus::Scanned { installed }
                }
                Err(message) => RootStatus::Unreadable { message },
            }
        } else {
            RootStatus::NotSelected
        };
        RootScan {
            agent: agent.kind(),
            path: agent.root().to_path_buf(),
            status: root_status_for(agent, status),
        }
    });

    InventorySnapshot {
        rows: assemble_rows(observations),
        roots,
    }
}

/// A root that does not exist is missing, not unreadable: absence is expected.
fn root_status_for(agent: &AgentDetection, status: RootStatus) -> RootStatus {
    match status {
        RootStatus::Unreadable { .. } if !agent.root().exists() => RootStatus::Missing,
        other => other,
    }
}

fn scan_root(
    agent: &AgentDetection,
    index: &ResolutionIndex,
    budget: &mut InspectionBudget,
) -> Result<Vec<InstalledSkillObservation>, String> {
    let root = agent.root();
    let metadata = fs::metadata(root).map_err(|error| error.to_string())?;
    if !metadata.is_dir() {
        return Err("the skill root is not a directory".to_owned());
    }

    let mut names = Vec::new();
    for entry in fs::read_dir(root).map_err(|error| error.to_string())? {
        if !budget.consume_entry() {
            return Err("the scan exceeded its bounded inspection limit".to_owned());
        }
        if names.len() == MAX_ROOT_CHILDREN {
            return Err(format!(
                "the skill root holds more than {MAX_ROOT_CHILDREN} entries"
            ));
        }
        let entry = entry.map_err(|error| error.to_string())?;
        names.push(entry.file_name());
    }
    names.sort();

    names
        .into_iter()
        .map(|name| observe(agent.kind(), root, &name, index, budget))
        .collect()
}

fn observe(
    agent: AgentKind,
    root: &Path,
    name: &std::ffi::OsStr,
    index: &ResolutionIndex,
    budget: &mut InspectionBudget,
) -> Result<InstalledSkillObservation, String> {
    let path = root.join(name);
    let name = name.to_string_lossy().into_owned();
    let file_type = fs::symlink_metadata(&path)
        .map_err(|error| error.to_string())?
        .file_type();

    if file_type.is_symlink() {
        let target = fs::read_link(&path).unwrap_or_default();
        let Ok(canonical) = path.canonicalize() else {
            // The target is carried on the observation, so the evidence states
            // the fact rather than repeating a path the detail pane already
            // shows in the reader's own notation.
            return Ok(broken_object(
                agent,
                name,
                path,
                InstallationObject::Symlink { target },
                Finding {
                    code: "install.dangling_symlink",
                    severity: FindingSeverity::Critical,
                    evidence: "the link target does not exist".to_owned(),
                },
            ));
        };
        let resolution = index.resolve(&canonical);
        return classify(
            agent,
            name,
            path,
            InstallationObject::Symlink { target },
            resolution,
            budget,
        );
    }

    if file_type.is_dir() {
        return classify(
            agent,
            name,
            path,
            InstallationObject::Directory,
            None,
            budget,
        );
    }

    Ok(broken_object(
        agent,
        name.clone(),
        path,
        InstallationObject::File,
        Finding {
            code: "skill.missing_skill_md",
            severity: FindingSeverity::Critical,
            evidence: format!("{name} is a file, so it holds no SKILL.md"),
        },
    ))
}

/// Validate the installed content and settle its state.
///
/// Validation runs through the installation path rather than the link target,
/// so the declared name is compared against the name the agent will load the
/// skill under.
fn classify(
    agent: AgentKind,
    name: String,
    path: PathBuf,
    object: InstallationObject,
    resolution: Option<VariantResolution>,
    budget: &mut InspectionBudget,
) -> Result<InstalledSkillObservation, String> {
    match validate_portable_skill_with_budget(&path, budget) {
        Ok(validated) => {
            let managed = resolution.is_some();
            Ok(InstalledSkillObservation {
                agent,
                name,
                path,
                object,
                resolution,
                validation: Some(SkillValidation::Valid {
                    name: validated.name().to_owned(),
                    description: validated.description().to_owned(),
                }),
                findings: if managed {
                    Vec::new()
                } else {
                    vec![Finding {
                        code: "install.unmanaged",
                        severity: FindingSeverity::Info,
                        evidence: "this installation does not come from a registered source"
                            .to_owned(),
                    }]
                },
                health: if managed {
                    InstallationHealth::Healthy
                } else {
                    InstallationHealth::Unmanaged
                },
            })
        }
        Err(PortableValidationError::SourceInspectionLimitExceeded) => {
            Err("the scan exceeded its bounded inspection limit".to_owned())
        }
        Err(error) => {
            let message = error.to_string();
            Ok(InstalledSkillObservation {
                agent,
                name,
                path,
                object,
                resolution,
                validation: Some(SkillValidation::Invalid {
                    message: message.clone(),
                }),
                findings: vec![Finding {
                    code: validation_finding_code(&error),
                    severity: FindingSeverity::Critical,
                    evidence: message,
                }],
                health: InstallationHealth::Broken,
            })
        }
    }
}

fn broken_object(
    agent: AgentKind,
    name: String,
    path: PathBuf,
    object: InstallationObject,
    finding: Finding,
) -> InstalledSkillObservation {
    InstalledSkillObservation {
        agent,
        name,
        path,
        object,
        resolution: None,
        validation: None,
        findings: vec![finding],
        health: InstallationHealth::Broken,
    }
}

/// The stable code for a portable-validation failure.
fn validation_finding_code(error: &PortableValidationError) -> &'static str {
    match error {
        PortableValidationError::ReadDirectory { .. }
        | PortableValidationError::UnreadableSkillMd(_) => "skill.unreadable",
        PortableValidationError::MissingSkillMd => "skill.missing_skill_md",
        PortableValidationError::SkillMdTooLarge { .. } => "skill.skill_md_too_large",
        PortableValidationError::SkillDirectoryTooLarge { .. } => "skill.directory_too_large",
        PortableValidationError::SourceInspectionLimitExceeded => "skill.inspection_limit",
        PortableValidationError::MissingFrontmatter
        | PortableValidationError::UnterminatedFrontmatter
        | PortableValidationError::InvalidFrontmatter(_) => "skill.invalid_frontmatter",
        PortableValidationError::InvalidName => "skill.invalid_name",
        PortableValidationError::NameMismatch { .. } => "skill.name_mismatch",
        PortableValidationError::InvalidDescription => "skill.invalid_description",
    }
}

fn assemble_rows(observations: Vec<InstalledSkillObservation>) -> Vec<InventoryRow> {
    let mut rows: Vec<InventoryRow> = Vec::new();
    for observation in observations {
        let position = match rows.iter().position(|row| row.name == observation.name) {
            Some(position) => position,
            None => {
                rows.push(InventoryRow {
                    name: observation.name.clone(),
                    observations: [const { None }; 3],
                    health: observation.health(),
                });
                rows.len() - 1
            }
        };
        let row = &mut rows[position];
        row.health = row.health.max(observation.health());
        let agent = observation.agent.index();
        row.observations[agent] = Some(observation);
    }
    rows.sort_by(|left, right| left.name.cmp(&right.name));
    rows
}

/// Canonical paths of every included catalog candidate of every registered
/// source.
///
/// Resolution is path equality against this index and nothing else, so content
/// that merely resembles a registered variant is never claimed as managed.
struct ResolutionIndex {
    by_canonical_path: HashMap<PathBuf, VariantResolution>,
}

impl ResolutionIndex {
    fn of(sources: &[RegisteredSource]) -> Self {
        let mut by_canonical_path = HashMap::new();
        for source in sources {
            for catalog in source
                .catalogs()
                .iter()
                .filter(|catalog| catalog.included())
            {
                for candidate in catalog.candidates() {
                    let Ok(canonical) = source
                        .git_top_level()
                        .join(candidate.relative_path())
                        .canonicalize()
                    else {
                        continue;
                    };
                    by_canonical_path
                        .entry(canonical)
                        .or_insert_with(|| VariantResolution {
                            source_id: source.id(),
                            source_label: source.label().to_owned(),
                            catalog_relative_path: catalog.relative_path().to_path_buf(),
                            variant_relative_path: candidate.relative_path().to_path_buf(),
                        });
                }
            }
        }
        Self { by_canonical_path }
    }

    fn resolve(&self, canonical: &Path) -> Option<VariantResolution> {
        self.by_canonical_path.get(canonical).cloned()
    }
}
