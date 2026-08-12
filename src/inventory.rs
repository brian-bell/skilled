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
    agents::{adapter, detection_at},
    resolution::{
        CandidateSelection, OpenCodeEntry, OpenCodeResolution, RootSighting, SightedEntry,
        VariantRef, narrow, resolve_opencode, variants_by_name,
    },
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
/// Severity orders findings for display. Whether a particular finding offers
/// repair is decided separately from the read-only inventory scan.
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
    /// State one observed fact, under a stable code.
    ///
    /// Findings are built here and in [`crate::operations`], which states the
    /// spec 18.2 collision codes over a plan rather than over a scan. The type
    /// is shared because the two say the same kind of thing to a reader, and
    /// Doctor's ordering already keys on the code.
    pub(crate) fn new(code: &'static str, severity: FindingSeverity, evidence: String) -> Self {
        Self {
            code,
            severity,
            evidence,
        }
    }

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

/// Where an installation's content actually is.
///
/// Kept apart from the object type because the two answer different questions:
/// the object says what occupies the slot, this says what — if anything — an
/// agent would load through it. The three cases are the same distinction the
/// rest of the module keeps: resolved, observed to resolve to nothing, and not
/// established. Only the last leaves an effective resolution unstated.
#[derive(Clone, Debug, Eq, PartialEq)]
enum ContentLocation {
    /// The slot resolves to this canonical directory.
    At(PathBuf),
    /// The slot was observed to resolve to nothing: a dangling link, or content
    /// that is not a skill directory at all.
    Nowhere,
    /// The slot could not be resolved, so what it holds is not known.
    Unknown,
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
    Resolved(VariantRef),
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
    content: ContentLocation,
    provenance: Provenance,
    validation: Option<SkillValidation>,
    findings: Vec<Finding>,
    health: InstallationHealth,
}

impl InstalledSkillObservation {
    pub fn agent(&self) -> AgentKind {
        self.agent
    }

    /// What this slot contributes to another agent's discovery of the name.
    ///
    /// The three cases the scan keeps apart survive the translation: content
    /// that resolves, content observed to resolve to nothing, and content whose
    /// resolution could not be established. The last is what stops an effective
    /// resolution being stated over it.
    fn sighting(&self) -> RootSighting {
        if !self.object.is_installation() {
            return RootSighting::NothingToLoad;
        }
        match &self.content {
            ContentLocation::At(canonical) => RootSighting::Offers(SightedEntry::new(
                self.path.clone(),
                canonical.clone(),
                self.resolution().cloned(),
            )),
            ContentLocation::Nowhere => RootSighting::NothingToLoad,
            ContentLocation::Unknown => RootSighting::Unknown,
        }
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

    pub fn resolution(&self) -> Option<&VariantRef> {
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

/// The single verdict a row's Health column states.
///
/// The declared order is the rule: a row takes the greatest of what its
/// installations are and what OpenCode's effective resolution makes of them.
/// A conflicting duplicate outranks everything, because an agent resolving the
/// wrong content is worse than an agent resolving nothing; incompatible and
/// foreign exposure rank below broken, because content OpenCode merely cannot
/// claim is a lesser fault than content no agent can load.
///
/// [`InstallationHealth`] stays the vocabulary of one installation. This is the
/// vocabulary of a row, which is the only place an effective resolution exists.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RowVerdict {
    NotASkill,
    Healthy,
    Unverified,
    Unmanaged,
    /// A registered variant is visible to OpenCode but excludes it.
    IncompatibleVariant,
    /// Another agent's agent-specific variant is all OpenCode can see.
    ForeignVariant,
    Broken,
    /// More than one directory answers to the name for one agent.
    Conflict,
}

impl RowVerdict {
    /// The word the Health column shows, and the filter matches against.
    ///
    /// `foreign` is the short form of the detail region's "foreign variant":
    /// the column is sized for the longest badge it can hold, and a word that
    /// forced it wider would take the columns from the skill and source names
    /// beside it. The detail region spells the state out in full.
    pub fn label(self) -> &'static str {
        match self {
            Self::NotASkill => "not a skill",
            Self::Healthy => "healthy",
            Self::Unverified => "unverified",
            Self::Unmanaged => "unmanaged",
            Self::IncompatibleVariant => "incompatible",
            Self::ForeignVariant => "foreign",
            Self::Broken => "broken",
            Self::Conflict => "conflict",
        }
    }

    fn of(health: InstallationHealth) -> Self {
        match health {
            InstallationHealth::NotASkill => Self::NotASkill,
            InstallationHealth::Healthy => Self::Healthy,
            InstallationHealth::Unverified => Self::Unverified,
            InstallationHealth::Unmanaged => Self::Unmanaged,
            InstallationHealth::Broken => Self::Broken,
        }
    }
}

/// One skill name, across every agent that carries it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InventoryRow {
    name: String,
    observations: [Option<InstalledSkillObservation>; 3],
    health: InstallationHealth,
    opencode_resolution: Option<OpenCodeResolution>,
    resolution_findings: Vec<Finding>,
}

impl InventoryRow {
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What OpenCode would load for this name, or `None` when OpenCode is not
    /// selected and its roots were therefore never resolved for it.
    ///
    /// Computed rather than observed: the roots OpenCode consults are the same
    /// three native roots the scan already read, so this costs no further
    /// filesystem work — which is also why a root the user asked Skilled to
    /// leave alone stays unread and leaves the answer
    /// [`OpenCodeResolution::Incomplete`].
    pub fn opencode_resolution(&self) -> Option<&OpenCodeResolution> {
        self.opencode_resolution.as_ref()
    }

    /// What the effective resolution found, as findings.
    ///
    /// Filed on the row rather than on an installation because that is what
    /// they are about: an alias, a conflict, and an exposure are all statements
    /// about several roots at once. Attaching one to each installation it
    /// concerns would state the same paragraph two and three times over in the
    /// one region that shows every finding it holds.
    pub fn resolution_findings(&self) -> &[Finding] {
        &self.resolution_findings
    }

    /// The one verdict the Health column states for this row.
    pub fn verdict(&self) -> RowVerdict {
        let installations = RowVerdict::of(self.health);
        match &self.opencode_resolution {
            Some(OpenCodeResolution::Conflict { .. }) => installations.max(RowVerdict::Conflict),
            Some(OpenCodeResolution::IncompatibleExposure { .. }) => {
                installations.max(RowVerdict::IncompatibleVariant)
            }
            Some(OpenCodeResolution::ForeignExposure { .. }) => {
                installations.max(RowVerdict::ForeignVariant)
            }
            _ => installations,
        }
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
            .chain(&self.resolution_findings)
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

    /// The same state, in the fewest words: the phrase surfaces that hold
    /// several roots at once — the setup scan step, the detail region's NOT
    /// READ section, and the OpenCode resolution's unread reasons — share
    /// this vocabulary so one state never has two names.
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

/// A registry-side ambiguity: more than one registered variant answers to one
/// name for one agent.
///
/// Kept apart from the findings that hang off observations because it can exist
/// with nothing installed at all — two repositories offering the same skill are
/// already ambiguous before anyone installs either — so there is no observation
/// for it to belong to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectionFinding {
    skill_name: String,
    agent: AgentKind,
    variants: Vec<VariantRef>,
    finding: Finding,
}

impl SelectionFinding {
    pub fn skill_name(&self) -> &str {
        &self.skill_name
    }

    pub fn agent(&self) -> AgentKind {
        self.agent
    }

    /// Every variant that survived the selection order, in its stable order.
    pub fn variants(&self) -> &[VariantRef] {
        &self.variants
    }

    pub fn finding(&self) -> &Finding {
        &self.finding
    }
}

/// One finding as Doctor lists it, with what it is a finding about.
#[derive(Clone, Copy, Debug)]
pub struct DoctorEntry<'a> {
    skill_name: &'a str,
    agent: AgentKind,
    finding: &'a Finding,
    observation: Option<&'a InstalledSkillObservation>,
    variants: &'a [VariantRef],
}

impl<'a> DoctorEntry<'a> {
    pub(crate) fn from_observation(
        skill_name: &'a str,
        finding: &'a Finding,
        observation: &'a InstalledSkillObservation,
    ) -> Self {
        Self {
            skill_name,
            agent: observation.agent(),
            finding,
            observation: Some(observation),
            variants: &[],
        }
    }
    pub fn skill_name(&self) -> &'a str {
        self.skill_name
    }

    /// The agent the finding concerns. Every finding has one: an observation
    /// belongs to the root it was read from, and a selection conflict is a
    /// conflict for the agent whose selection it defeated.
    pub fn agent(&self) -> AgentKind {
        self.agent
    }

    pub fn finding(&self) -> &'a Finding {
        self.finding
    }

    /// The installation the finding was observed on, where there is one.
    pub fn observation(&self) -> Option<&'a InstalledSkillObservation> {
        self.observation
    }

    /// The competing variants, where the finding is a selection conflict.
    pub fn variants(&self) -> &'a [VariantRef] {
        self.variants
    }

    /// Whether this finding is about the registry rather than about what is
    /// installed.
    ///
    /// The two findings that share `variant.duplicate_for_agent` are told apart
    /// by this and nothing else: only a selection conflict carries the variants
    /// that defeated it, because only the registry has variants to compete.
    /// Anything that must state which of the two it is asks here rather than
    /// re-deriving it from the shape of the entry.
    pub fn concerns_the_registry(&self) -> bool {
        !self.variants.is_empty()
    }
}

/// Which of the spec 9.5 groups a finding belongs to, lowest first.
///
/// Doctor is issue-first and grouped before it is sorted by severity, so a
/// stable code that is not listed here would silently sort last rather than
/// wrongly: the fallback is the informational group, which is where an
/// unclassified observation belongs until someone places it deliberately.
pub(crate) fn doctor_order(code: &str) -> u8 {
    match code {
        // 1. Broken or dangling installations.
        "install.dangling_symlink"
        | "install.unresolvable_symlink"
        | "install.unreadable_entry" => 0,
        // 2. Conflicting duplicates and incorrect effective resolution.
        "variant.duplicate_for_agent" | "install.wrong_managed_target" => 1,
        // 3. Invalid SKILL.md content or frontmatter.
        code if code.starts_with("skill.") => 2,
        // 4. Wrong, foreign, or explicitly incompatible agent variant.
        "variant.foreign_opencode_exposure" | "variant.incompatible_for_opencode" => 3,
        // 5 and 6 — disabled skills, blocked updates — have no codes yet.
        // 7. Missing or ambiguous provenance.
        "install.provenance_unverified" => 6,
        // 8. Unmanaged installations and informational benign aliases. Stray
        // content is listed here too: an agent ignores it, so it is the same
        // class of observation as an alias that costs nothing.
        "install.unmanaged" | "install.not_a_skill" | "variant.benign_alias" => 7,
        // A stable code nobody has placed sorts with the informational group
        // rather than ahead of findings that were placed deliberately.
        _ => 7,
    }
}

/// Everything one read-only pass over the native roots observed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InventorySnapshot {
    rows: Vec<InventoryRow>,
    roots: [RootScan; 3],
    selection_findings: Vec<SelectionFinding>,
    registry_is_complete: bool,
}

impl InventorySnapshot {
    pub(crate) fn observation_at(&self, path: &Path) -> Option<(&str, &InstalledSkillObservation)> {
        self.rows.iter().find_map(|row| {
            row.observations()
                .find(|observation| observation.path() == path)
                .map(|observation| (row.name(), observation))
        })
    }
    pub fn rows(&self) -> &[InventoryRow] {
        &self.rows
    }

    pub fn selection_findings(&self) -> &[SelectionFinding] {
        &self.selection_findings
    }

    /// Whether every registered source and included catalog could be read.
    ///
    /// A checkout that has moved away contributes no variants, so no ambiguity
    /// between it and another can be found. That is the same kind of gap an
    /// unreadable root leaves in the roots, and it is kept for the same
    /// reason: a list assembled over part of the registry may not be presented
    /// as an account of all of it.
    pub fn registry_is_complete(&self) -> bool {
        self.registry_is_complete
    }

    /// Every finding, in the order Doctor lists them.
    ///
    /// Grouped by the spec 9.5 order, then by severity within a group, then by
    /// skill name and agent so two runs over the same filesystem produce the
    /// same list.
    pub fn doctor_findings(&self) -> impl Iterator<Item = DoctorEntry<'_>> {
        let mut entries: Vec<DoctorEntry<'_>> = self
            .rows
            .iter()
            .flat_map(|row| {
                row.observations()
                    .flat_map(move |observation| {
                        observation
                            .findings()
                            .iter()
                            .map(move |finding| DoctorEntry {
                                skill_name: row.name(),
                                agent: observation.agent(),
                                finding,
                                observation: Some(observation),
                                variants: &[],
                            })
                    })
                    // An effective resolution is OpenCode's, and belongs to no
                    // one root: it is filed against the agent whose resolution
                    // it describes rather than against a root it crosses.
                    .chain(
                        row.resolution_findings()
                            .iter()
                            .map(move |finding| DoctorEntry {
                                skill_name: row.name(),
                                agent: AgentKind::OpenCode,
                                finding,
                                observation: None,
                                variants: &[],
                            }),
                    )
            })
            .chain(self.selection_findings.iter().map(|selection| DoctorEntry {
                skill_name: selection.skill_name(),
                agent: selection.agent(),
                finding: selection.finding(),
                observation: None,
                variants: selection.variants(),
            }))
            .collect();
        entries.sort_by_key(|entry| {
            (
                doctor_order(entry.finding.code()),
                std::cmp::Reverse(entry.finding.severity()),
                entry.skill_name,
                entry.agent.index(),
            )
        });
        entries.into_iter()
    }

    /// How many findings the scan holds, whatever may be said about them.
    ///
    /// Counted rather than listed: order is what [`Self::doctor_findings`]
    /// spends a sort on, and a count has no use for it. Every surface that only
    /// needs to know whether there is one, or how many, asks here.
    pub fn finding_count(&self) -> usize {
        self.rows
            .iter()
            .map(|row| {
                row.observations()
                    .map(|observation| observation.findings().len())
                    .sum::<usize>()
                    + row.resolution_findings().len()
            })
            .sum::<usize>()
            + self.selection_findings.len()
    }

    /// The number of findings, when the scan is entitled to state one.
    ///
    /// Findings are observations, so the verdict that gates the skill count
    /// gates this too: the Doctor tab, its subtitle, and its empty state
    /// cannot disagree because all three ask here.
    ///
    /// Findings come from a second place the skill count does not depend on.
    /// A registry Skilled could not read in full may hold the very variant
    /// that would have made a name ambiguous, so a number stated over it would
    /// claim a completeness the scan never had.
    pub fn stated_finding_count(&self) -> Option<usize> {
        (self.registry_is_complete)
            .then(|| self.stated_skill_count().map(|_| self.finding_count()))
            .flatten()
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

    /// The snapshot of a session that has not read any selected root yet.
    ///
    /// First-run setup reads the installation roots at its own step, after the
    /// user has chosen which agents Skilled should configure. Until then there
    /// is nothing to report, and this says so rather than reporting an empty
    /// result that would look like a scan finding nothing.
    ///
    /// Deselected agents are already terminal for this pass: the scan that
    /// lands will leave them [`RootStatus::NotSelected`] and never touch them,
    /// so the gap must say the same rather than claim they are waiting to be
    /// read.
    pub(crate) fn not_scanned(agents: &[AgentDetection; 3]) -> Self {
        Self {
            rows: Vec::new(),
            selection_findings: Vec::new(),
            // Nothing has been read, so nothing has been found wanting: the
            // count is withheld by the roots being unread, not by the registry.
            registry_is_complete: true,
            roots: agents.each_ref().map(|agent| RootScan {
                agent: agent.kind(),
                path: agent.root().to_path_buf(),
                status: if agent.selected() {
                    RootStatus::NotScanned
                } else {
                    RootStatus::NotSelected
                },
            }),
        }
    }

    /// Every observation that is actually shaped like an installation.
    ///
    /// A stray file beside the skill directories is listed so the user can see
    /// it, but it is not an installation and may not be counted as one: a
    /// `.DS_Store` must not inflate what the inventory subtitle and the setup
    /// summary report.
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

    /// The number of installed skills, when the scan is entitled to state one.
    ///
    /// This is the single place that decides whether a number may be shown at
    /// all, so every surface that summarises the roots reaches the same
    /// verdict.
    ///
    /// Two conditions must hold. Every root Skilled was asked to look at was
    /// read or found absent, so the number covers the whole requested scope —
    /// that is [`Self::counts_are_complete`]. And at least one root was
    /// actually read: finding every root absent entitles the application to a
    /// phrase about the roots, not to a measurement of their contents. The
    /// difference is the one between "no root read" and "nothing installed",
    /// and a bare `0` beside a tab would flatten it.
    pub fn stated_skill_count(&self) -> Option<usize> {
        let read_a_root = self
            .roots
            .iter()
            .any(|root| matches!(root.status(), RootStatus::Scanned { .. }));
        (self.counts_are_complete() && read_a_root).then(|| self.skill_row_count())
    }

    /// Whether the scan has not started and at least one selected root is still
    /// waiting to be read.
    ///
    /// [`RootStatus::NotSelected`] is allowed beside pending roots: those were
    /// never in scope and must not block the "not scanned" claim for the ones
    /// that are. Any real observation — scanned, missing, or unreadable —
    /// means the pass has begun. All-deselected is not pending: nothing will
    /// be read — that case is [`Self::no_agent_configured`].
    pub fn scan_pending(&self) -> bool {
        let mut any_pending = false;
        for root in &self.roots {
            match root.status() {
                RootStatus::NotScanned => any_pending = true,
                RootStatus::NotSelected => {}
                RootStatus::Scanned { .. }
                | RootStatus::Missing
                | RootStatus::Unreadable { .. } => return false,
            }
        }
        any_pending
    }

    /// Whether every root was left alone because no agent is configured.
    ///
    /// Peer claim to [`Self::scan_pending`]: nothing is waiting to be read and
    /// nothing was observed, because the user chose no agents. Surfaces that
    /// summarise the roots ask this once rather than re-deriving the all-
    /// [`RootStatus::NotSelected`] check.
    pub fn no_agent_configured(&self) -> bool {
        self.roots
            .iter()
            .all(|root| root.status() == &RootStatus::NotSelected)
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

    let mut rows = assemble_rows(observations);
    // OpenCode's effective resolution is settled here, where every root's
    // observations for a name are together for the first time and the root
    // statuses say which of them were actually read. A deselected OpenCode is
    // asked nothing: the resolution belongs to an agent that was left alone.
    if detection_at(agents, AgentKind::OpenCode).selected() {
        let rule = opencode_precedence_rule();
        for row in &mut rows {
            let resolution = resolve_opencode(root_sightings(row, &roots));
            row.resolution_findings = opencode_findings(row.name(), &resolution, &rule);
            row.opencode_resolution = Some(resolution);
        }
    }
    let registry_is_complete = whole_registry(sources);
    let selection_findings = selection_findings(agents, sources, &rows, registry_is_complete);

    InventorySnapshot {
        rows,
        roots,
        selection_findings,
        registry_is_complete,
    }
}

/// What each native root contributes to a name, for the agents that read it.
///
/// A root that was not read in full contributes no sighting at all — not an
/// absence — because the difference between the two is the difference between
/// a selection and a conflict. A root found missing was read: it holds nothing.
fn root_sightings(row: &InventoryRow, roots: &[RootScan; 3]) -> [RootSighting; 3] {
    AgentKind::ALL.map(|agent| match roots[agent.index()].status() {
        RootStatus::Scanned { .. } => row.observation(agent).map_or(
            RootSighting::NothingToLoad,
            InstalledSkillObservation::sighting,
        ),
        RootStatus::Missing => RootSighting::NothingToLoad,
        RootStatus::NotScanned | RootStatus::NotSelected | RootStatus::Unreadable { .. } => {
            RootSighting::Unread
        }
    })
}

/// The precedence rule, spelled out for a finding's evidence.
///
/// Read from the adapter rather than restated, so the sentence a user is shown
/// cannot drift from the order the code actually applies.
fn opencode_precedence_rule() -> String {
    let opencode = adapter(AgentKind::OpenCode);
    let roots: Vec<String> = std::iter::once(opencode.native_skill_root())
        .chain(opencode.compatibility_skill_roots().iter().copied())
        .map(|root| format!("~/{root}"))
        .collect();
    format!("OpenCode reads {}, in that order", roots.join(", then "))
}

/// One installation as the user speaks about it: an agent's documented global
/// root, and the name inside it.
///
/// Built from the adapter rather than from the observed absolute path. The
/// reader knows these roots by their documented spelling, a finding that
/// carried a home directory would repeat what the detail region already shows
/// in its own notation, and the phrase is then the same on every machine.
fn root_relative_path(root: AgentKind, name: &str) -> String {
    format!("~/{}/{name}", adapter(root).native_skill_root())
}

fn joined_paths<'a>(entries: impl IntoIterator<Item = &'a OpenCodeEntry>, name: &str) -> String {
    entries
        .into_iter()
        .map(|entry| root_relative_path(entry.root(), name))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What the effective resolution has to say, as findings on the row.
///
/// One finding per fact: a conflict is one conflict however many roots are
/// party to it, and its evidence names them all.
fn opencode_findings(name: &str, resolution: &OpenCodeResolution, rule: &str) -> Vec<Finding> {
    match resolution {
        // An unread root is not a classification, and the Doctor list is a list
        // of findings rather than of gaps; the detail region says so in words.
        OpenCodeResolution::NothingVisible | OpenCodeResolution::Incomplete { .. } => Vec::new(),
        OpenCodeResolution::Selected { winner, aliases } if !aliases.is_empty() => {
            let others = joined_paths(aliases, name);
            vec![Finding {
                code: "variant.benign_alias",
                severity: FindingSeverity::Info,
                evidence: format!(
                    "the same directory is visible to OpenCode through {others} as well as \
                     {}; {rule}, so it loads one skill and not several",
                    root_relative_path(winner.root(), name)
                ),
            }]
        }
        OpenCodeResolution::Selected { .. } => Vec::new(),
        OpenCodeResolution::ForeignExposure { winner, aliases } => {
            let variant = winner
                .variant()
                .map(VariantRef::evidence_label)
                .unwrap_or_default();
            vec![Finding {
                code: "variant.foreign_opencode_exposure",
                severity: FindingSeverity::Warning,
                evidence: format!(
                    "{} resolves to {variant}, which is another agent's edition: its \
                     catalog is laid out under that agent's own root. {rule}, so it sees \
                     this name with no edition of its own behind it.",
                    joined_paths(std::iter::once(winner).chain(aliases), name)
                ),
            }]
        }
        OpenCodeResolution::IncompatibleExposure { winner, aliases } => {
            let variant = winner
                .variant()
                .map(VariantRef::evidence_label)
                .unwrap_or_default();
            vec![Finding {
                code: "variant.incompatible_for_opencode",
                severity: FindingSeverity::Warning,
                evidence: format!(
                    "{} resolves to {variant}, whose registered catalog is not registered for \
                     OpenCode. {rule}, so OpenCode can reach this name but Skilled does not \
                     claim the variant is usable by it.",
                    joined_paths(std::iter::once(winner).chain(aliases), name)
                ),
            }]
        }
        OpenCodeResolution::Conflict { entries } => {
            let paths = joined_paths(entries, name);
            let winner = entries
                .first()
                .map(|entry| root_relative_path(entry.root(), name))
                .unwrap_or_default();
            vec![Finding {
                code: "variant.duplicate_for_agent",
                severity: FindingSeverity::Critical,
                evidence: format!(
                    "OpenCode finds this name in more than one root, and they are different \
                     directories: {paths}; {rule}, so it would load {winner}"
                ),
            }]
        }
    }
}

/// Whether every registered source, and every catalog Skilled was asked to
/// include, could be read this pass.
pub(crate) fn whole_registry(sources: &[RegisteredSource]) -> bool {
    sources.iter().all(|source| {
        source.source_error().is_none()
            && source
                .catalogs()
                .iter()
                .filter(|catalog| catalog.included())
                .all(|catalog| catalog.scan_error().is_none())
    })
}

/// Every registry-side ambiguity, for the agents Skilled was asked to manage.
///
/// One finding per agent and name, not one per competing variant: the
/// ambiguity is a single fact about a selection, and the variants that caused
/// it are its evidence.
fn selection_findings(
    agents: &[AgentDetection; 3],
    sources: &[RegisteredSource],
    rows: &[InventoryRow],
    // A source or catalog that could not be read contributed no candidates, so
    // the variants listed below are the ones Skilled could see rather than
    // every variant there is. The evidence says so rather than presenting a
    // partial registry as a complete one.
    whole_registry: bool,
) -> Vec<SelectionFinding> {
    let by_name = variants_by_name(sources);
    let mut findings = Vec::new();
    for (name, variants) in &by_name {
        for agent in AgentKind::ALL {
            if !detection_at(agents, agent).selected() {
                continue;
            }
            let CandidateSelection::Duplicate(variants) = narrow(variants, agent) else {
                continue;
            };
            let row = rows.iter().find(|row| row.name() == *name);
            // An ambiguity nothing has resolved yet leaves a future
            // installation uncertain; one an installation already answers to
            // means an agent is loading content Skilled cannot say was chosen.
            // An observation is not an installation: a stray file is ignored by
            // the agent, and an entry the scan could not read is one this
            // module refuses to claim either way. OpenCode is the exception to
            // looking only at its native observation: a settled effective
            // resolution may load an installation through a compatibility
            // root, which makes the ambiguity current for OpenCode too.
            let installed = row.is_some_and(|row| {
                row.observation(agent)
                    .is_some_and(|observation| observation.object().is_installation())
                    || (agent == AgentKind::OpenCode
                        && matches!(
                            row.opencode_resolution(),
                            Some(
                                OpenCodeResolution::Selected { .. }
                                    | OpenCodeResolution::ForeignExposure { .. }
                                    | OpenCodeResolution::IncompatibleExposure { .. }
                                    | OpenCodeResolution::Conflict { .. }
                            )
                        ))
            });
            let listed = variants
                .iter()
                .map(VariantRef::evidence_label)
                .collect::<Vec<_>>()
                .join(", ");
            let scope = if whole_registry {
                ""
            } else {
                ", of the sources that could be read"
            };
            // The fact that raises this to critical is named, because a row
            // reading `critical` beside one reading `warning` under the same
            // code and the same words would leave the difference unaccounted
            // for.
            let standing = if installed {
                format!(
                    " {} already has an installation under this name, so what it \
                     resolves to is ambiguous now rather than at the next install.",
                    agent.display_name()
                )
            } else {
                String::new()
            };
            findings.push(SelectionFinding {
                skill_name: (*name).to_owned(),
                agent,
                finding: Finding {
                    code: "variant.duplicate_for_agent",
                    severity: if installed {
                        FindingSeverity::Critical
                    } else {
                        FindingSeverity::Warning
                    },
                    evidence: format!(
                        "{} has more than one registered variant of {name}{scope}: {listed}. \
                         The selection order — an exact agent-specific variant, then a \
                         compatible common one — does not narrow it to one.{standing}",
                        agent.display_name()
                    ),
                },
                variants,
            });
        }
    }
    findings
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
                    content: ContentLocation::Unknown,
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
                        // A dangling link resolves to nothing, which is an
                        // observation rather than a gap: an agent reading this
                        // root loads nothing under the name.
                        content: ContentLocation::Nowhere,
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
                        content: ContentLocation::Unknown,
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
                content: ContentLocation::At(canonical.clone()),
            },
            resolution,
            Some(canonical),
            index.accounts_for_every_source,
            budget,
        );
    }

    if file_type.is_dir() {
        // A physical directory is its own content. Canonicalizing it still
        // matters: another root may reach the very same directory through a
        // link, and only the resolved path can show that the two are one.
        let content = path
            .canonicalize()
            .map_or(ContentLocation::Unknown, ContentLocation::At);
        return classify(
            Slot {
                agent,
                name,
                path,
                object: InstallationObject::Directory,
                content,
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
        content: ContentLocation::Nowhere,
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
    resolution: Option<VariantRef>,
    resolved_from: Option<PathBuf>,
    accountable: bool,
    budget: &mut InspectionBudget,
) -> Result<InstalledSkillObservation, String> {
    let Slot {
        agent,
        name,
        path,
        object,
        content,
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
                content,
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
                // Content that fails the portable core is not what any agent
                // resolves this name to, and content Skilled could not read is
                // not something it can say either way. Keeping the resolved
                // directory here would let a skill no agent can load answer for
                // the name in another agent's effective resolution — the same
                // asymmetry `select_candidates` refuses on the registry side.
                content: unloadable_content(&error),
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
fn provenance_of(resolution: &Option<VariantRef>, accountable: bool) -> Provenance {
    match (resolution, accountable) {
        (Some(resolution), _) => Provenance::Resolved(resolution.clone()),
        (None, true) => Provenance::Unregistered,
        (None, false) => Provenance::Unverified,
    }
}

/// The identity of one installation slot, and where its content resolved to,
/// before anything else is known about it.
struct Slot {
    agent: AgentKind,
    name: String,
    path: PathBuf,
    object: InstallationObject,
    content: ContentLocation,
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
        content: slot.content,
        provenance,
        validation: None,
        findings: vec![finding],
        health: InstallationHealth::Broken,
    }
}

/// Where content that failed validation leaves an agent looking for the name.
///
/// A failure Skilled could read is a failure it can state: the portable core
/// is what an agent loads a skill by, so the name resolves to nothing. A
/// failure it could not read through — a directory it was denied, a document
/// too large to inspect, one whose bytes are not text — is not a verdict about
/// the content at all. The last of those is conservative: an agent would not
/// load it either, but the error that carries it also carries the failures
/// Skilled genuinely could not see through, and withholding a verdict is the
/// safe side of that ambiguity.
fn unloadable_content(error: &PortableValidationError) -> ContentLocation {
    match error {
        PortableValidationError::ReadDirectory { .. }
        | PortableValidationError::UnreadableSkillMd(_)
        | PortableValidationError::SkillMdTooLarge { .. }
        | PortableValidationError::SkillDirectoryTooLarge { .. }
        | PortableValidationError::SourceInspectionLimitExceeded => ContentLocation::Unknown,
        PortableValidationError::MissingSkillMd
        | PortableValidationError::MissingFrontmatter
        | PortableValidationError::UnterminatedFrontmatter
        | PortableValidationError::InvalidFrontmatter(_)
        | PortableValidationError::InvalidName
        | PortableValidationError::NameMismatch { .. }
        | PortableValidationError::InvalidDescription => ContentLocation::Nowhere,
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
                    // Settled after the rows exist: an effective resolution is
                    // a statement about every root at once, which no single
                    // observation is in a position to make.
                    opencode_resolution: None,
                    resolution_findings: Vec::new(),
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
    by_canonical_path: HashMap<PathBuf, VariantRef>,
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
                        .or_insert_with(|| VariantRef::of(source, catalog, candidate));
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

    fn resolve(&self, canonical: &Path) -> Option<VariantRef> {
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

    /// The pre-scan snapshot must not claim a deselected root is waiting to be
    /// read. The scan that lands will leave it NotSelected and never touch it,
    /// so the gap must say the same rather than flash "not scanned" first.
    #[test]
    fn not_scanned_marks_deselected_agents_not_selected() {
        let temporary = tempfile::tempdir().expect("temporary home");
        let home = temporary.path().join("home");
        let environment = AppEnvironment::new(&home, temporary.path().join("data"), "");
        let mut agents = detect_agents(&environment);
        agents[AgentKind::Codex.index()].set_selected(false);

        let snapshot = InventorySnapshot::not_scanned(&agents);

        assert!(snapshot.rows().is_empty());
        assert_eq!(
            snapshot.root(AgentKind::ClaudeCode).status(),
            &RootStatus::NotScanned
        );
        assert_eq!(
            snapshot.root(AgentKind::Codex).status(),
            &RootStatus::NotSelected
        );
        assert_eq!(
            snapshot.root(AgentKind::OpenCode).status(),
            &RootStatus::NotScanned
        );
        assert_eq!(snapshot.stated_skill_count(), None);
        // A mixed gap is still pending: selected roots have not been read.
        assert!(snapshot.scan_pending());
    }

    /// "Not scanned" UI may fire when deselected roots sit beside pending ones,
    /// but not when every agent was deselected — nothing is waiting to be read.
    #[test]
    fn scan_pending_tolerates_not_selected_and_rejects_observations() {
        let temporary = tempfile::tempdir().expect("temporary home");
        let home = temporary.path().join("home");
        let environment = AppEnvironment::new(&home, temporary.path().join("data"), "");
        let mut agents = detect_agents(&environment);

        let all_selected = InventorySnapshot::not_scanned(&agents);
        assert!(all_selected.scan_pending());

        for agent in &mut agents {
            agent.set_selected(false);
        }
        let none_selected = InventorySnapshot::not_scanned(&agents);
        assert!(!none_selected.scan_pending());
        assert!(none_selected.no_agent_configured());
        assert_eq!(none_selected.stated_skill_count(), None);
        assert!(!all_selected.no_agent_configured());

        // Any real observation means the scan has begun, even when some agents
        // stay deselected beside the ones that were read or found absent.
        agents[AgentKind::ClaudeCode.index()].set_selected(true);
        let missing = scan_with_budget(&agents, &[], InspectionBudget::installation_scan());
        assert!(matches!(
            missing.root(AgentKind::ClaudeCode).status(),
            RootStatus::Missing
        ));
        assert_eq!(
            missing.root(AgentKind::Codex).status(),
            &RootStatus::NotSelected
        );
        assert!(!missing.scan_pending());

        // A root that exists but cannot be listed is an observation too.
        let blocked = home.join(".claude/skills");
        fs::create_dir_all(home.join(".claude")).expect("create Claude Code parent");
        fs::write(&blocked, "not a directory").expect("block the root with a file");
        let unreadable = scan_with_budget(&agents, &[], InspectionBudget::installation_scan());
        assert!(matches!(
            unreadable.root(AgentKind::ClaudeCode).status(),
            RootStatus::Unreadable { .. }
        ));
        assert!(!unreadable.scan_pending());

        // So is a root that was read in full.
        fs::remove_file(&blocked).expect("clear the blocking file");
        fs::create_dir_all(&blocked).expect("create an empty root");
        let scanned = scan_with_budget(&agents, &[], InspectionBudget::installation_scan());
        assert_eq!(
            scanned.root(AgentKind::ClaudeCode).status(),
            &RootStatus::Scanned { installed: 0 }
        );
        assert!(!scanned.scan_pending());
    }

    /// Finding every root absent is a complete answer about the roots, but it
    /// is not a measurement of what any root holds: nothing was read. Only a
    /// root that was actually read entitles the application to a number.
    #[test]
    fn a_count_needs_a_root_that_was_read_and_not_merely_one_found_absent() {
        let temporary = tempfile::tempdir().expect("temporary home");
        let home = temporary.path().join("home");
        let environment = AppEnvironment::new(&home, temporary.path().join("data"), "");
        let agents = detect_agents(&environment);

        let absent = scan_with_budget(&agents, &[], InspectionBudget::installation_scan());

        assert!(
            absent
                .roots()
                .iter()
                .all(|root| root.status() == &RootStatus::Missing)
        );
        // The scope was covered, so a phrase about the roots is honest ...
        assert!(absent.counts_are_complete());
        // ... but there is no observation to state as a number.
        assert_eq!(absent.stated_skill_count(), None);

        // An empty root that exists was read, and zero is what it holds.
        fs::create_dir_all(home.join(".claude/skills")).expect("create an empty root");
        let read = scan_with_budget(&agents, &[], InspectionBudget::installation_scan());

        assert_eq!(read.stated_skill_count(), Some(0));
    }
}
