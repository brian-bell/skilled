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
    fs, io,
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
    /// A regular file, a socket, a device node — anything a skill cannot be.
    NotADirectory,
    /// The entry exists but its type could not be read.
    Unknown,
}

impl InstallationObject {
    pub fn description(&self) -> &'static str {
        match self {
            Self::Symlink { .. } => "symbolic link",
            Self::Directory => "directory",
            Self::NotADirectory => "not a directory",
            // Saying "file" here would assert a type the scan never read.
            Self::Unknown => "could not be read",
        }
    }

    /// Whether this object occupies an installation slot at all.
    ///
    /// A stray file plainly does not, and counting it would inflate both the
    /// per-root count and the setup summary. An entry Skilled could not read
    /// might, and is counted, because understating what is installed is the
    /// worse error.
    pub fn is_installation(&self) -> bool {
        !matches!(self, Self::NotADirectory)
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
    /// The registered source's stable identity.
    ///
    /// Labels come from the checkout's directory name, so two repositories can
    /// share one. Anything deciding whether two installations came from the
    /// same source must compare this.
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

/// The state of an installation.
///
/// The declared order is the roll-up rule: a row takes the greatest state of
/// the agents that carry it, so any broken installation makes the row broken
/// and any unmanaged one outranks a healthy one. Content that is not a skill
/// ranks lowest, because a name that is a real skill somewhere and a stray
/// file elsewhere is best described by the skill.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum InstallationHealth {
    /// Not a skill at all: a file, a socket, a device node.
    NotASkill,
    /// A registered variant, installed as a link, that validates.
    Healthy,
    /// Structurally sound, but Skilled could not establish where it came from
    /// because a registered source could not be read.
    Unverified,
    /// Structurally sound content Skilled does not own.
    Unmanaged,
    /// Content an agent cannot load.
    Broken,
}

impl InstallationHealth {
    pub fn label(self) -> &'static str {
        match self {
            Self::NotASkill => "not a skill",
            Self::Healthy => "healthy",
            Self::Unverified => "unverified",
            Self::Unmanaged => "unmanaged",
            Self::Broken => "broken",
        }
    }
}

/// Where one installation came from.
///
/// Recorded rather than inferred, so it survives every outcome: an
/// installation that fails validation still has whatever provenance the scan
/// was able to establish, and one whose provenance could not be established
/// says so instead of defaulting to "from nowhere registered".
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Provenance {
    /// It came from this registered source variant.
    Resolved(VariantResolution),
    /// Every registered source was accounted for and none contains it.
    Unregistered,
    /// A registered source could not be read, so this is not known.
    Unverified,
}

impl Provenance {
    /// The word the filter matches this installation's provenance against.
    ///
    /// A row that mixes provenances answers a query with the provenance of
    /// each installation it holds, not only with its single summary word.
    pub fn label(&self) -> &str {
        match self {
            Self::Resolved(resolution) => resolution.source_label(),
            Self::Unregistered => "not registered",
            Self::Unverified => "unverified",
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
    provenance: Provenance,
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

    pub fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    pub fn resolution(&self) -> Option<&VariantResolution> {
        match &self.provenance {
            Provenance::Resolved(resolution) => Some(resolution),
            _ => None,
        }
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

    /// Where this row's installations came from, taken together.
    ///
    /// A single answer is only given when every resolved installation agrees.
    /// Two agents carrying the same name from different sources is a real
    /// arrangement, and naming one of them would misstate the other; which of
    /// them ought to win is a conflict a later slice decides.
    pub fn provenance(&self) -> RowProvenance<'_> {
        // A row of nothing but stray content holds no installation, so where
        // one came from is not a question it can answer; "not registered"
        // would state a provenance fact about something that has none.
        if self
            .observations()
            .all(|observation| !observation.object().is_installation())
        {
            return RowProvenance::NotApplicable;
        }
        // One unknown is enough: a row summary that named the source of the
        // installations it could place would imply the same source for the one
        // it could not.
        if self
            .observations()
            .any(|observation| observation.provenance() == &Provenance::Unverified)
        {
            return RowProvenance::Unverified;
        }
        let mut resolved = self
            .observations()
            .filter_map(|observation| observation.resolution());
        let Some(first) = resolved.next() else {
            return RowProvenance::Unregistered;
        };
        // A valid installation that resolved to no registered source, sitting
        // beside one that did: naming the source would claim the unmanaged
        // installation too. Broken content takes no position — a dangling
        // link may be the moved-away remains of the very source the resolved
        // installation names — and stray content is not an installation.
        if self.observations().any(|observation| {
            observation.provenance() == &Provenance::Unregistered
                && matches!(
                    observation.validation(),
                    Some(SkillValidation::Valid { .. })
                )
        }) {
            return RowProvenance::Mixed;
        }
        if resolved.all(|resolution| resolution.source_id() == first.source_id()) {
            RowProvenance::Source(first.source_label())
        } else {
            RowProvenance::Divergent
        }
    }

    pub fn findings(&self) -> impl Iterator<Item = &Finding> {
        self.observations()
            .flat_map(|observation| observation.findings())
    }

    /// Whether this row is a skill installation, rather than other content a
    /// root happens to hold beside its skills.
    ///
    /// A directory or a symbolic link is one, whether or not it currently
    /// works: a dangling link is a broken installation, not stray content, and
    /// the health column says so. An entry Skilled could not read is not
    /// claimed as an installation either way.
    pub fn is_skill(&self) -> bool {
        self.observations().any(|observation| {
            matches!(
                observation.object(),
                InstallationObject::Symlink { .. } | InstallationObject::Directory
            )
        })
    }
}

/// Where a row's installations came from, taken together.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RowProvenance<'a> {
    /// The row holds no installation at all — only stray content — so no
    /// source question applies.
    NotApplicable,
    /// No installation resolved, and a registered source could not be read, so
    /// whether they came from one is not known.
    Unverified,
    /// No installation resolved to a registered source.
    Unregistered,
    /// Every resolved installation came from this source.
    Source(&'a str),
    /// Some installations resolved to a registered source and others to none,
    /// so no single answer describes the row.
    Mixed,
    /// Resolved installations came from more than one source.
    Divergent,
}

impl RowProvenance<'_> {
    /// The word the Source column shows, and the filter matches against.
    ///
    /// A row that no source question applies to shows nothing: any word in
    /// the column would be an answer to that question.
    pub fn label(&self) -> &str {
        match self {
            Self::NotApplicable => "",
            Self::Unverified => "unverified",
            Self::Unregistered => "not registered",
            Self::Source(label) => label,
            Self::Mixed => "mixed",
            Self::Divergent => "multiple sources",
        }
    }
}

/// What Skilled was able to observe about one agent's native root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RootStatus {
    /// Nothing has been read yet. Setup reads the roots at its own step.
    NotScanned,
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
            Self::NotScanned => "not scanned yet".to_owned(),
            Self::Unreadable { .. } => "root unreadable".to_owned(),
            other => other.short_summary(),
        }
    }

    /// The same state, for a line that has to hold all three roots at once.
    pub fn short_summary(&self) -> String {
        match self {
            Self::NotScanned => "not scanned".to_owned(),
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
        self.installations().count()
    }

    pub fn unmanaged_count(&self) -> usize {
        self.installations()
            .filter(|observation| observation.health() == InstallationHealth::Unmanaged)
            .count()
    }

    /// Installation slots an agent cannot load.
    ///
    /// The setup summary reports this rather than a total finding count, which
    /// would double-report every unmanaged install through the informational
    /// finding that marks it.
    pub fn broken_count(&self) -> usize {
        self.installations()
            .filter(|observation| observation.health() == InstallationHealth::Broken)
            .count()
    }

    /// The snapshot of a session that has not read any root yet.
    ///
    /// First-run setup reads the installation roots at its own step, after the
    /// user has chosen which agents Skilled should configure. Until then there
    /// is nothing to report, and this says so rather than reporting an empty
    /// result that would look like a scan finding nothing.
    pub(crate) fn not_scanned(agents: &[AgentDetection; 3]) -> Self {
        Self {
            rows: Vec::new(),
            roots: agents.each_ref().map(|agent| RootScan {
                agent: agent.kind(),
                path: agent.root().to_path_buf(),
                status: RootStatus::NotScanned,
            }),
        }
    }

    /// Every observation that is actually shaped like an installation.
    ///
    /// A stray file beside the skill directories is listed so the user can see
    /// it, but it is not an installation and may not be counted as one: a
    /// `.DS_Store` must not inflate what the Roots line and the setup summary
    /// report.
    fn installations(&self) -> impl Iterator<Item = &InstalledSkillObservation> {
        self.rows
            .iter()
            .flat_map(InventoryRow::observations)
            .filter(|observation| observation.object().is_installation())
    }

    /// Rows that are skills, as opposed to other content the roots also hold.
    ///
    /// An entry whose type could not be read is not counted: it might be a
    /// skill, and the subtitle beside this count says "skills" outright.
    pub fn skill_row_count(&self) -> usize {
        self.rows.iter().filter(|row| row.is_skill()).count()
    }

    /// Whether the counts taken across the roots describe the whole picture.
    ///
    /// A count is an observation. It may only be stated when every root
    /// Skilled was asked to look at was either read or found absent — not when
    /// one could not be read, and not when it was never asked to look at any.
    pub fn counts_are_complete(&self) -> bool {
        self.roots.iter().all(|root| {
            matches!(
                root.status(),
                RootStatus::Scanned { .. } | RootStatus::Missing | RootStatus::NotSelected
            )
        }) && self.roots.iter().any(|root| {
            matches!(
                root.status(),
                RootStatus::Scanned { .. } | RootStatus::Missing
            )
        })
    }

    /// The message behind an unreadable root, if any root could not be read.
    ///
    /// A root Skilled could not read in full contributes nothing, so the reason
    /// is the only account the user gets of what is in it.
    pub fn unreadable_roots(&self) -> impl Iterator<Item = (&RootScan, &str)> {
        self.roots.iter().filter_map(|root| match root.status() {
            RootStatus::Unreadable { message } => Some((root, message.as_str())),
            _ => None,
        })
    }
}

/// Observe every selected agent's native root once.
pub(crate) fn scan_installations(
    agents: &[AgentDetection; 3],
    sources: &[RegisteredSource],
) -> InventorySnapshot {
    scan_with_budget(agents, sources, InspectionBudget::installation_scan())
}

fn scan_with_budget(
    agents: &[AgentDetection; 3],
    sources: &[RegisteredSource],
    mut budget: InspectionBudget,
) -> InventorySnapshot {
    const EXHAUSTED: &str = "the scan exceeded its bounded inspection limit";

    let (index, complete) = ResolutionIndex::of(sources, &mut budget);
    let mut observations = Vec::new();
    let roots = agents.each_ref().map(|agent| {
        // Selection is checked first: a root the user asked Skilled to leave
        // alone stays left alone whatever else went wrong.
        let status = if !agent.selected() {
            RootStatus::NotSelected
        } else if !complete {
            // An index that ran out of budget is missing registered variants,
            // and an installation pointing at one of those would be reported
            // as belonging to no source at all. Rather than invent that
            // provenance, nothing is reported from any root.
            RootStatus::Unreadable {
                message: EXHAUSTED.to_owned(),
            }
        } else {
            match scan_root(agent, &index, &mut budget) {
                Ok(found) => {
                    let installed = found
                        .iter()
                        .filter(|observation| observation.object().is_installation())
                        .count();
                    observations.extend(found);
                    RootStatus::Scanned { installed }
                }
                Err(message) => RootStatus::Unreadable { message },
            }
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
///
/// Only absence qualifies. A root Skilled was denied — an unsearchable parent,
/// say — was not observed to be missing, and reporting it as missing would
/// both assert something unobserved and suppress the reason the scan failed.
/// The check is on the link itself, so a root that is a dangling symbolic link
/// is something the user put there and stays unreadable.
fn root_status_for(agent: &AgentDetection, status: RootStatus) -> RootStatus {
    let RootStatus::Unreadable { .. } = status else {
        // Nothing else needs the distinction, and a deselected root must not
        // be touched even to ask whether it exists.
        return status;
    };
    let absent = matches!(
        fs::symlink_metadata(agent.root()),
        Err(error) if error.kind() == io::ErrorKind::NotFound
    );
    if absent { RootStatus::Missing } else { status }
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
    let file_type = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata.file_type(),
        // One entry that cannot be read is one entry, not a failed root. An
        // agent writing into its own skill directory mid-scan would otherwise
        // erase everything Skilled had already observed there.
        Err(error) => {
            return Ok(broken_object(
                Slot {
                    agent,
                    name,
                    path,
                    object: InstallationObject::Unknown,
                },
                Provenance::Unverified,
                Finding {
                    code: "install.unreadable_entry",
                    severity: FindingSeverity::Critical,
                    evidence: format!("the entry could not be read: {error}"),
                },
            ));
        }
    };

    if file_type.is_symlink() {
        let target = fs::read_link(&path).unwrap_or_default();
        let canonical = match path.canonicalize() {
            Ok(canonical) => canonical,
            // A link can fail to resolve for reasons other than a missing
            // target — a denied directory along the way, a symlink cycle, an
            // over-long name. Only absence is a dangling link; anything else
            // says what actually happened rather than asserting a cause
            // Skilled did not observe. The target itself is carried on the
            // observation, so neither evidence repeats a path the detail pane
            // already shows in the reader's own notation.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(broken_object(
                    Slot {
                        agent,
                        name,
                        path,
                        object: InstallationObject::Symlink { target },
                    },
                    // A link into a checkout that has since been moved away is
                    // dangling and unplaceable at once; claiming it came from
                    // no registered source would be the second thing Skilled
                    // cannot know about it.
                    provenance_of(&None, index.accounts_for_every_source),
                    Finding {
                        code: "install.dangling_symlink",
                        severity: FindingSeverity::Critical,
                        evidence: "the link target does not exist".to_owned(),
                    },
                ));
            }
            Err(error) => {
                return Ok(broken_object(
                    Slot {
                        agent,
                        name,
                        path,
                        object: InstallationObject::Symlink { target },
                    },
                    Provenance::Unverified,
                    Finding {
                        code: "install.unresolvable_symlink",
                        severity: FindingSeverity::Critical,
                        evidence: format!("the link target could not be resolved: {error}"),
                    },
                ));
            }
        };
        // The canonical path resolved here and the one portable validation
        // resolves for itself are two separate reads, so a target swapped
        // between them could carry a source label that no longer describes the
        // content. The scan is read-only and the window requires write access
        // to the agent root, so it is recorded rather than locked against.
        let resolution = index.resolve(&canonical);
        return classify(
            Slot {
                agent,
                name,
                path,
                object: InstallationObject::Symlink { target },
            },
            resolution,
            Some(canonical),
            index.accounts_for_every_source,
            budget,
        );
    }

    if file_type.is_dir() {
        return classify(
            Slot {
                agent,
                name,
                path,
                object: InstallationObject::Directory,
            },
            None,
            None,
            index.accounts_for_every_source,
            budget,
        );
    }

    // A regular file, a socket, or a device node beside the skill directories
    // is not a broken installation — an agent simply ignores it. Reporting it
    // as critical would manufacture alarm out of a stray README or .DS_Store,
    // so it is observed as content that is not a skill and left alone.
    Ok(InstalledSkillObservation {
        agent,
        name,
        path,
        object: InstallationObject::NotADirectory,
        provenance: Provenance::Unregistered,
        validation: None,
        findings: vec![Finding {
            code: "install.not_a_skill",
            severity: FindingSeverity::Info,
            evidence: "this is not a directory, so it cannot hold a SKILL.md".to_owned(),
        }],
        health: InstallationHealth::NotASkill,
    })
}

/// Validate the installed content and settle its state.
///
/// Validation runs through the installation path rather than the link target,
/// so the declared name is compared against the name the agent will load the
/// skill under.
fn classify(
    slot: Slot,
    resolution: Option<VariantResolution>,
    resolved_from: Option<PathBuf>,
    accountable: bool,
    budget: &mut InspectionBudget,
) -> Result<InstalledSkillObservation, String> {
    let Slot {
        agent,
        name,
        path,
        object,
    } = slot;
    match validate_portable_skill_with_budget(&path, budget) {
        Ok(validated) => {
            // Resolution and validation each canonicalized the path for
            // themselves. A link swapped between the two would carry a source
            // label describing content that is no longer there, so the target
            // is confirmed to be the one that was resolved before the label is
            // kept. A mismatch is not an error — it is simply not a resolution.
            let resolution = match resolved_from {
                Some(expected) if path.canonicalize().ok().as_ref() != Some(&expected) => None,
                _ => resolution,
            };
            // Absence from the index only proves non-membership when the index
            // accounts for every registered source.
            let state = match (&resolution, accountable) {
                (Some(_), _) => None,
                (None, true) => Some((
                    InstallationHealth::Unmanaged,
                    Finding {
                        code: "install.unmanaged",
                        severity: FindingSeverity::Info,
                        evidence: "this installation does not come from a registered source"
                            .to_owned(),
                    },
                )),
                (None, false) => Some((
                    InstallationHealth::Unverified,
                    Finding {
                        code: "install.provenance_unverified",
                        severity: FindingSeverity::Warning,
                        evidence: "a registered source could not be read, so Skilled cannot \
                                   tell whether this came from one"
                            .to_owned(),
                    },
                )),
            };
            Ok(InstalledSkillObservation {
                agent,
                name,
                path,
                object,
                provenance: provenance_of(&resolution, accountable),
                validation: Some(SkillValidation::Valid {
                    name: validated.name().to_owned(),
                    description: validated.description().to_owned(),
                }),
                findings: state.iter().map(|(_, finding)| finding.clone()).collect(),
                health: state.map_or(InstallationHealth::Healthy, |(health, _)| health),
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
                // A failed validation says nothing about where the content
                // came from, so whatever the scan established is kept.
                provenance: provenance_of(&resolution, accountable),
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

/// What the scan was able to establish about where an installation came from.
fn provenance_of(resolution: &Option<VariantResolution>, accountable: bool) -> Provenance {
    match (resolution, accountable) {
        (Some(resolution), _) => Provenance::Resolved(resolution.clone()),
        (None, true) => Provenance::Unregistered,
        (None, false) => Provenance::Unverified,
    }
}

/// The identity of one installation slot, before anything is known about it.
struct Slot {
    agent: AgentKind,
    name: String,
    path: PathBuf,
    object: InstallationObject,
}

fn broken_object(
    slot: Slot,
    provenance: Provenance,
    finding: Finding,
) -> InstalledSkillObservation {
    InstalledSkillObservation {
        agent: slot.agent,
        name: slot.name,
        path: slot.path,
        object: slot.object,
        provenance,
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
    // Keyed rather than searched: three full roots can carry thousands of names
    // apiece, and a linear probe per observation would square that.
    let mut positions: HashMap<String, usize> = HashMap::new();
    for observation in observations {
        let position = match positions.get(&observation.name) {
            // Names are compared as displayed, so two entries holding
            // different invalid byte sequences can collide. Occupying the slot
            // a second time starts a new row rather than overwriting one:
            // understating what is installed is the error this module exists
            // to avoid.
            Some(position) if rows[*position].observations[observation.agent.index()].is_none() => {
                *position
            }
            _ => {
                rows.push(InventoryRow {
                    name: observation.name.clone(),
                    observations: [const { None }; 3],
                    health: observation.health(),
                });
                positions.insert(observation.name.clone(), rows.len() - 1);
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
    /// Whether every registered source could be accounted for.
    ///
    /// A source whose checkout is unavailable, or a catalog whose scan failed,
    /// contributes no candidates. Absence from the index is then not evidence
    /// of non-membership, and an installation that fails to resolve cannot be
    /// called unmanaged.
    accounts_for_every_source: bool,
}

impl ResolutionIndex {
    /// Build the index, charging each resolution to the scan's budget.
    ///
    /// The candidate count comes from what the user registered, so this walk
    /// is bounded like every other filesystem walk in the module rather than
    /// trusted to be small. Returns whether the index is complete: a partial
    /// one cannot be used to decide that anything is unmanaged, because the
    /// entry that would have resolved it may be one of the ones missing.
    fn of(sources: &[RegisteredSource], budget: &mut InspectionBudget) -> (Self, bool) {
        let mut by_canonical_path = HashMap::new();
        let mut accounts_for_every_source = sources.iter().all(|source| {
            source.source_error().is_none()
                && source
                    .catalogs()
                    .iter()
                    .filter(|catalog| catalog.included())
                    .all(|catalog| catalog.scan_error().is_none())
        });
        for source in sources {
            // Candidate paths were validated when the source was registered,
            // but the filesystem has moved on since. A candidate that now
            // resolves outside its checkout — because a directory along the
            // way became a link elsewhere — is not that source's content, and
            // adopting it would let anything outside the source be reported as
            // managed by it.
            let Ok(canonical_source) = source.git_top_level().canonicalize() else {
                accounts_for_every_source = false;
                continue;
            };
            for catalog in source
                .catalogs()
                .iter()
                .filter(|catalog| catalog.included())
            {
                for candidate in catalog.candidates() {
                    if !budget.consume_entry() {
                        return (
                            Self {
                                by_canonical_path,
                                accounts_for_every_source,
                            },
                            false,
                        );
                    }
                    let Ok(canonical) = source
                        .git_top_level()
                        .join(candidate.relative_path())
                        .canonicalize()
                    else {
                        continue;
                    };
                    if !canonical.starts_with(&canonical_source) {
                        accounts_for_every_source = false;
                        continue;
                    }
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
        (
            Self {
                by_canonical_path,
                accounts_for_every_source,
            },
            true,
        )
    }

    fn resolve(&self, canonical: &Path) -> Option<VariantResolution> {
        self.by_canonical_path.get(canonical).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppEnvironment, agents::detect_agents};

    /// A scan whose resolution index could not be finished cannot decide that
    /// anything is unmanaged, because the entry that would have resolved an
    /// installation may be one of the ones the budget cut off. It must
    /// therefore report nothing at all rather than false provenance.
    ///
    /// The production budget is far too large to exhaust from a fixture, so
    /// the invariant is pinned here rather than left to emerge from the order
    /// in which the scan happens to spend it.
    #[test]
    fn an_unfinished_resolution_index_reports_no_root_as_scanned() {
        let temporary = tempfile::tempdir().expect("temporary home");
        let home = temporary.path().join("home");
        let root = home.join(".claude/skills/installed");
        fs::create_dir_all(&root).expect("create an installed skill");
        fs::write(
            root.join("SKILL.md"),
            "---\nname: installed\ndescription: fixture\n---\n# Installed\n",
        )
        .expect("write SKILL.md");
        let environment = AppEnvironment::new(&home, temporary.path().join("data"), "");
        let agents = detect_agents(&environment);

        let snapshot = scan_with_budget(&agents, &[], InspectionBudget::exhausted());

        assert!(
            snapshot.rows().is_empty(),
            "an unfinished index must not produce rows"
        );
        // A root that exists cannot be reported as read. One that does not
        // exist is still absent, which the index has no bearing on.
        assert!(matches!(
            snapshot.root(AgentKind::ClaudeCode).status(),
            RootStatus::Unreadable { .. }
        ));
        for root in snapshot.roots() {
            assert!(
                !matches!(root.status(), RootStatus::Scanned { .. }),
                "{:?} claimed a scan result: {:?}",
                root.agent(),
                root.status()
            );
        }
    }

    /// The same scan with a whole budget sees the installation, so the test
    /// above is measuring the budget and not a broken fixture.
    #[test]
    fn the_same_fixture_is_reported_when_the_budget_is_whole() {
        let temporary = tempfile::tempdir().expect("temporary home");
        let home = temporary.path().join("home");
        let root = home.join(".claude/skills/installed");
        fs::create_dir_all(&root).expect("create an installed skill");
        fs::write(
            root.join("SKILL.md"),
            "---\nname: installed\ndescription: fixture\n---\n# Installed\n",
        )
        .expect("write SKILL.md");
        let environment = AppEnvironment::new(&home, temporary.path().join("data"), "");
        let agents = detect_agents(&environment);

        let snapshot = scan_with_budget(&agents, &[], InspectionBudget::installation_scan());

        assert_eq!(snapshot.rows().len(), 1);
        assert_eq!(snapshot.rows()[0].name(), "installed");
        assert_eq!(
            snapshot.root(AgentKind::ClaudeCode).status(),
            &RootStatus::Scanned { installed: 1 }
        );
    }
}
