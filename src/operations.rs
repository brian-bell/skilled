//! Install planning and its guarded execution.
//!
//! Spec 17.2 reserves this module for pure plan builders and guarded executors,
//! and the split is kept literally: [`probe_install`] is the only read of the
//! machine, [`plan_install`] decides everything over the value it returns, and
//! nothing here writes until [`apply_install`] is called with a plan the user
//! has confirmed.
//!
//! Two rules shape the whole module. A plan blocks whole rather than in part —
//! if any target is blocked, nothing is written anywhere, because spec 15 asks
//! Skilled to stop before writing when it already knows a step would fail. And
//! nothing is ever replaced: this release creates links and creates them only,
//! so every occupied slot is a refusal rather than an overwrite, and repair is
//! a later slice's work.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::{
    AgentDetection, AgentKind,
    agents::detection_at,
    inventory::{Finding, FindingSeverity},
    resolution::{
        CandidateSelection, OpenCodeResolution, RootSighting, SightedEntry, VariantRef, narrow,
        resolve_opencode, variants_by_name,
    },
    source::RegisteredSource,
    validation::{InspectionBudget, PortableValidationError, validate_portable_skill_with_budget},
};

/// Why an install request could not be turned into a plan at all.
///
/// These are failures of identity rather than of the machine: there is no
/// target to state anything about, so there is no plan to show. Everything the
/// filesystem has to say about a target lives in the plan itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlanFailure {
    /// The variant is no longer one the registry offers under this name — its
    /// source or catalog became unreadable, or it stopped validating.
    VariantUnavailable { skill_name: String },
    /// The variant's own directory could not be resolved inside its checkout.
    SourceUnavailable { reason: String },
}

impl std::fmt::Display for PlanFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VariantUnavailable { skill_name } => write!(
                formatter,
                "no registered source still offers a usable variant named {skill_name}"
            ),
            Self::SourceUnavailable { reason } => {
                write!(
                    formatter,
                    "the variant directory could not be read: {reason}"
                )
            }
        }
    }
}

/// Why an install request names no single variant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocateFailure {
    /// No registered source carries that identifier.
    UnknownSource { source_id: i64 },
    /// The source is registered but offers no usable variant of that name.
    NoSuchSkill { skill_name: String },
    /// Several variants inside one source answer to the name, so the request
    /// does not say which. Every candidate is named so the caller can.
    Ambiguous {
        skill_name: String,
        variants: Vec<VariantRef>,
    },
}

impl std::fmt::Display for LocateFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownSource { source_id } => {
                write!(formatter, "no registered source has id {source_id}")
            }
            Self::NoSuchSkill { skill_name } => write!(
                formatter,
                "that source offers no usable variant named {skill_name}"
            ),
            Self::Ambiguous {
                skill_name,
                variants,
            } => write!(
                formatter,
                "that source offers {} variants named {skill_name}: {}",
                variants.len(),
                variants
                    .iter()
                    .map(VariantRef::evidence_label)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

/// Find the one variant a request names, inside one registered source.
///
/// `catalog` narrows to a single catalog root where the caller already knows
/// which one it means — the Sources pane focuses an exact row — and is left
/// unset where it does not, which is where the ambiguity a request cannot
/// resolve becomes visible.
pub fn locate_variant(
    sources: &[RegisteredSource],
    source_id: i64,
    catalog: Option<&Path>,
    skill_name: &str,
) -> Result<VariantRef, LocateFailure> {
    if !sources.iter().any(|source| source.id() == source_id) {
        return Err(LocateFailure::UnknownSource { source_id });
    }
    let mut matches: Vec<VariantRef> = variants_by_name(sources)
        .remove(skill_name)
        .unwrap_or_default()
        .into_iter()
        .filter(|variant| variant.source_id() == source_id)
        .filter(|variant| catalog.is_none_or(|path| variant.catalog_relative_path() == path))
        .collect();
    match matches.len() {
        0 => Err(LocateFailure::NoSuchSkill {
            skill_name: skill_name.to_owned(),
        }),
        1 => Ok(matches.remove(0)),
        _ => Err(LocateFailure::Ambiguous {
            skill_name: skill_name.to_owned(),
            variants: matches,
        }),
    }
}

/// What one agent's installation slot holds right now.
///
/// The final component is never followed: a symbolic link is observed as a
/// link, which is the difference between "Skilled already installed this" and
/// "something else is standing here".
#[derive(Clone, Debug, Eq, PartialEq)]
enum EntryProbe {
    /// Nothing occupies the slot.
    Absent,
    /// A symbolic link, with the target as recorded and where it resolves to.
    Symlink {
        target: PathBuf,
        canonical: Option<PathBuf>,
    },
    /// A physical directory, and where it resolves to. The resolved path
    /// matters even here: another root may reach the very same directory
    /// through a link, and only the canonical path shows that the two are one.
    Directory { canonical: Option<PathBuf> },
    /// A regular file, a socket, a device node — anything a skill cannot be.
    NotADirectory,
    /// The slot exists but could not be read.
    Unreadable(String),
}

/// What the agent's own global skill root holds.
#[derive(Clone, Debug, Eq, PartialEq)]
enum RootProbe {
    Present,
    /// The root does not exist. Whether the plan may create it turns on its
    /// parent: spec 15 has Skilled create the documented root and nothing
    /// above it, so an agent whose own directory is absent is left alone.
    Missing {
        parent_present: bool,
    },
    Unreadable(String),
}

/// What an agent would load through a slot, as distinct from what occupies it.
///
/// The same distinction [`crate::inventory`] keeps, and for the same reason: an
/// unreadable file blocks an install however unloadable it is, while a
/// directory that fails the portable core is not what any agent resolves the
/// name to and so cannot compete with anything for it.
#[derive(Clone, Debug, Eq, PartialEq)]
enum SlotContent {
    At(PathBuf),
    Nowhere,
    Unknown,
}

/// One agent's slot, as one read of the machine found it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetProbe {
    agent: AgentKind,
    link_path: PathBuf,
    root: RootProbe,
    entry: EntryProbe,
    content: SlotContent,
}

impl TargetProbe {
    pub fn agent(&self) -> AgentKind {
        self.agent
    }

    pub fn link_path(&self) -> &Path {
        &self.link_path
    }
}

/// Everything about the machine one install request depends on.
///
/// Taken in a single pass so the plan a user confirms describes one moment
/// rather than a sequence of reads that drifted apart. It is read again
/// immediately before each write; see [`apply_install`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallProbe {
    source_dir: Result<PathBuf, String>,
    targets: [TargetProbe; 3],
}

impl InstallProbe {
    pub fn target(&self, agent: AgentKind) -> &TargetProbe {
        &self.targets[agent.index()]
    }
}

/// Read the machine once, for one variant.
pub fn probe_install(
    agents: &[AgentDetection; 3],
    sources: &[RegisteredSource],
    variant: &VariantRef,
) -> InstallProbe {
    InstallProbe {
        source_dir: probe_source_dir(sources, variant),
        targets: agents
            .each_ref()
            .map(|agent| probe_target(agent, variant.skill_name())),
    }
}

/// The one canonical directory a variant names, confirmed to be inside its own
/// checkout.
///
/// Containment is checked rather than assumed for the reason
/// [`crate::inventory`] checks it: a directory along the way may have become a
/// link elsewhere since the source was registered, and a plan that linked to
/// whatever now sits at the end of that path would install content the source
/// does not contain.
fn probe_source_dir(sources: &[RegisteredSource], variant: &VariantRef) -> Result<PathBuf, String> {
    let source = sources
        .iter()
        .find(|source| source.id() == variant.source_id())
        .ok_or_else(|| "its source is no longer registered".to_owned())?;
    let checkout = source
        .git_top_level()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let directory = source
        .git_top_level()
        .join(variant.variant_relative_path())
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if !directory.starts_with(&checkout) {
        return Err(format!(
            "it resolves to {}, which is outside its checkout",
            directory.display()
        ));
    }
    Ok(directory)
}

fn probe_target(agent: &AgentDetection, skill_name: &str) -> TargetProbe {
    let link_path = agent.root().join(skill_name);
    let entry = probe_entry(&link_path);
    TargetProbe {
        root: probe_root(agent.root()),
        content: probe_content(&link_path, &entry),
        entry,
        agent: agent.kind(),
        link_path,
    }
}

/// What an agent reading this root would load under the name.
///
/// Validation runs through the installation path rather than through the link
/// target, exactly as the scanner runs it, so the declared name is compared
/// against the name the agent would load the skill under.
fn probe_content(link_path: &Path, entry: &EntryProbe) -> SlotContent {
    let canonical = match entry {
        EntryProbe::Absent | EntryProbe::NotADirectory => return SlotContent::Nowhere,
        EntryProbe::Unreadable(_) => return SlotContent::Unknown,
        EntryProbe::Symlink { canonical, .. } | EntryProbe::Directory { canonical } => canonical,
    };
    let Some(canonical) = canonical else {
        // A link that resolves to nothing loads nothing. That is an
        // observation, not a gap.
        return SlotContent::Nowhere;
    };
    let mut budget = InspectionBudget::installation_scan();
    match validate_portable_skill_with_budget(link_path, &mut budget) {
        Ok(_) => SlotContent::At(canonical.clone()),
        // Content Skilled could not read through is not a verdict about the
        // content, so it withholds one; content that plainly fails the portable
        // core is not what an agent resolves the name to.
        Err(
            PortableValidationError::ReadDirectory { .. }
            | PortableValidationError::UnreadableSkillMd(_)
            | PortableValidationError::SkillMdTooLarge { .. }
            | PortableValidationError::SkillDirectoryTooLarge { .. }
            | PortableValidationError::SourceInspectionLimitExceeded,
        ) => SlotContent::Unknown,
        Err(_) => SlotContent::Nowhere,
    }
}

fn probe_root(root: &Path) -> RootProbe {
    match fs::metadata(root) {
        Ok(metadata) if metadata.is_dir() => RootProbe::Present,
        Ok(_) => RootProbe::Unreadable("the skill root is not a directory".to_owned()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // Absence is only absence when the root itself is not there. A
            // dangling link standing where the root should be is something the
            // user put there, and creating through it is not this slice's work.
            if fs::symlink_metadata(root).is_ok() {
                return RootProbe::Unreadable("the skill root does not resolve".to_owned());
            }
            RootProbe::Missing {
                parent_present: root
                    .parent()
                    .is_some_and(|parent| fs::metadata(parent).is_ok_and(|meta| meta.is_dir())),
            }
        }
        Err(error) => RootProbe::Unreadable(error.to_string()),
    }
}

fn probe_entry(link_path: &Path) -> EntryProbe {
    let file_type = match fs::symlink_metadata(link_path) {
        Ok(metadata) => metadata.file_type(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return EntryProbe::Absent,
        Err(error) => return EntryProbe::Unreadable(error.to_string()),
    };
    if file_type.is_symlink() {
        return EntryProbe::Symlink {
            target: fs::read_link(link_path).unwrap_or_default(),
            canonical: link_path.canonicalize().ok(),
        };
    }
    if file_type.is_dir() {
        return EntryProbe::Directory {
            canonical: link_path.canonicalize().ok(),
        };
    }
    EntryProbe::NotADirectory
}

/// Why a target is not part of the work, without anything being wrong with it.
///
/// Kept apart from a blocking finding because these are the arrangement the
/// user asked for: an agent they left unconfigured, or one whose own edition of
/// the skill already outranks this one. A plan holding only exclusions is not
/// executable, and says so, but it is not a failure either.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExcludedReason {
    /// The user did not configure Skilled to manage this agent.
    NotConfigured,
    /// The request named other agents.
    NotRequested,
    /// The variant is not one this agent could use.
    Incompatible,
    /// The agent resolves this name to a different registered variant — spec
    /// 11.1 prefers an exact agent-specific edition — so installing this one
    /// would leave the agent loading something else.
    AgentSpecificOverride { selected: VariantRef },
}

/// What the plan will do about one agent's slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TargetDisposition {
    /// The root is there and the slot is free: create the link.
    CreateLink,
    /// The slot is free and the documented root is not there: create the root,
    /// one level and no more, then the link.
    CreateRootAndLink,
    /// The slot already resolves to this very directory, so there is nothing
    /// to write. `managed` says whether Skilled holds a receipt for it: an
    /// identical link it did not create stays unowned, because adopting one is
    /// a later slice's decision to make.
    AlreadyInstalled { managed: bool },
    /// Not part of the work, for a reason the user chose.
    Excluded { reason: ExcludedReason },
    /// Something occupies the slot, or stands in the way of it. Nothing is
    /// written anywhere while a plan holds one of these.
    Blocked { finding: Finding },
}

/// One agent's slot, and what the plan will do about it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallTarget {
    agent: AgentKind,
    link_path: PathBuf,
    disposition: TargetDisposition,
}

impl InstallTarget {
    pub fn agent(&self) -> AgentKind {
        self.agent
    }

    /// The absolute path the link would occupy.
    ///
    /// Absolute, and shown absolute: spec 15 has the preview state exactly what
    /// is about to be written, which an abbreviation against the home directory
    /// would soften.
    pub fn link_path(&self) -> &Path {
        &self.link_path
    }

    pub fn disposition(&self) -> &TargetDisposition {
        &self.disposition
    }

    /// Whether this target is one [`apply_install`] would write.
    pub fn is_work(&self) -> bool {
        matches!(
            self.disposition,
            TargetDisposition::CreateLink | TargetDisposition::CreateRootAndLink
        )
    }
}

/// One immutable statement of everything an install would do.
///
/// Built only by [`plan_install`], and never modified afterwards: what the user
/// confirmed is what gets applied, and the machine is re-read at apply time
/// rather than the plan being edited to match it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallPlan {
    variant: VariantRef,
    source_dir: PathBuf,
    targets: Vec<InstallTarget>,
    warnings: Vec<String>,
}

impl InstallPlan {
    pub fn skill_name(&self) -> &str {
        self.variant.skill_name()
    }

    pub fn variant(&self) -> &VariantRef {
        &self.variant
    }

    /// The one canonical directory every created link will point at.
    pub fn source_dir(&self) -> &Path {
        &self.source_dir
    }

    /// Every target, in [`AgentKind::ALL`] order.
    pub fn targets(&self) -> &[InstallTarget] {
        &self.targets
    }

    pub fn target(&self, agent: AgentKind) -> Option<&InstallTarget> {
        self.targets.iter().find(|target| target.agent == agent)
    }

    /// Observations that do not stop the work, but that the user should see
    /// before confirming it.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Every finding that stops this plan, with the agent it concerns.
    pub fn blocking_findings(&self) -> impl Iterator<Item = (AgentKind, &Finding)> {
        self.targets
            .iter()
            .filter_map(|target| match &target.disposition {
                TargetDisposition::Blocked { finding } => Some((target.agent, finding)),
                _ => None,
            })
    }

    pub fn is_blocked(&self) -> bool {
        self.blocking_findings().next().is_some()
    }

    /// Whether applying this plan would write anything.
    ///
    /// A plan with any blocked target is not executable however much work the
    /// other targets hold: spec 15 stops before the first write rather than
    /// part way through, when the failure is already known. A plan whose
    /// targets are all excluded or already installed is not executable either,
    /// and the preview says which of the two it is.
    pub fn is_executable(&self) -> bool {
        !self.is_blocked() && self.targets.iter().any(InstallTarget::is_work)
    }
}

/// Decide what installing one variant would do, over one probe.
///
/// Pure: every fact about the machine arrives in `probe`, and every fact about
/// the registry in `sources`. `requested` is the agent set the request named,
/// indexed like [`AgentKind::index`] — the Sources flow requests every
/// configured agent, and `skilled install --agents` requests exactly what it
/// was given.
pub fn plan_install(
    agents: &[AgentDetection; 3],
    sources: &[RegisteredSource],
    variant: &VariantRef,
    requested: [bool; 3],
    probe: &InstallProbe,
    receipts: &[Receipt],
) -> Result<InstallPlan, PlanFailure> {
    let competing = variants_by_name(sources)
        .remove(variant.skill_name())
        .unwrap_or_default();
    // The registry is re-read here rather than trusted from when the row was
    // focused: a variant that stopped validating, or whose catalog became
    // unreadable, is no longer something an agent would resolve to, and
    // installing it would be installing content Skilled cannot vouch for.
    if !competing.contains(variant) {
        return Err(PlanFailure::VariantUnavailable {
            skill_name: variant.skill_name().to_owned(),
        });
    }
    let source_dir = probe
        .source_dir
        .clone()
        .map_err(|reason| PlanFailure::SourceUnavailable { reason })?;

    let mut targets: Vec<InstallTarget> = AgentKind::ALL
        .into_iter()
        .map(|agent| {
            let probe = probe.target(agent);
            InstallTarget {
                agent,
                link_path: probe.link_path.clone(),
                disposition: disposition_for(
                    detection_at(agents, agent),
                    variant,
                    requested[agent.index()],
                    &competing,
                    probe,
                    &source_dir,
                    receipts,
                ),
            }
        })
        .collect();

    let mut warnings = plan_warnings(sources, variant);
    if detection_at(agents, AgentKind::OpenCode).selected() {
        apply_opencode_prediction(&mut targets, probe, variant, &source_dir, &mut warnings);
    }

    Ok(InstallPlan {
        variant: variant.clone(),
        source_dir,
        targets,
        warnings,
    })
}

/// State what OpenCode would resolve the name to once the plan has run.
///
/// OpenCode reads Claude Code's and Codex's roots as well as its own, so a
/// plan that touches any of them changes what OpenCode loads. Two outcomes are
/// possible and they are not treated alike.
///
/// A link written into OpenCode's own root that would *not* be what OpenCode
/// then resolves is refused. Its postcondition is already visibly false, and
/// spec 15 stops before writing rather than writing and reporting a failure.
///
/// Everything else is said rather than refused. Installing another agent's
/// edition into that agent's own root leaves OpenCode able to see content it
/// cannot use — but that is the arrangement the user asked for, and the same
/// arrangement the inventory reports as exposure rather than as breakage. The
/// design this slice was planned against refused those too; refusing them would
/// make an ordinary Claude Code install fail because OpenCode happens to read
/// that directory, which is not a decision Skilled should be making for anyone.
/// A conflict that already exists is likewise only restated: it is not this
/// plan's doing, and the user has Doctor for it.
fn apply_opencode_prediction(
    targets: &mut [InstallTarget],
    probe: &InstallProbe,
    variant: &VariantRef,
    source_dir: &Path,
    warnings: &mut Vec<String>,
) {
    let index = AgentKind::OpenCode.index();
    if matches!(
        targets[index].disposition,
        TargetDisposition::Blocked { .. }
    ) {
        // Its own finding is the more direct account of the same slot.
        return;
    }
    let predicted = resolve_opencode(sightings(targets, probe, variant, source_dir, true));
    if targets[index].is_work() {
        let settled = matches!(
            &predicted,
            OpenCodeResolution::Selected { winner, .. } if winner.root() == AgentKind::OpenCode
        );
        if !settled {
            targets[index].disposition = TargetDisposition::Blocked {
                finding: Finding::new(
                    "install.opencode_conflict",
                    FindingSeverity::Critical,
                    format!(
                        "OpenCode would not resolve {} to this link: {}",
                        variant.skill_name(),
                        predicted_summary(&predicted)
                    ),
                ),
            };
        }
        return;
    }
    let current = resolve_opencode(sightings(targets, probe, variant, source_dir, false));
    if predicted_kind(&predicted) != predicted_kind(&current)
        && let Some(concern) = opencode_concern(&predicted)
    {
        warnings.push(format!(
            "after this install, OpenCode {concern} for {}",
            variant.skill_name()
        ));
    }
}

/// What each native root would hold under the name, before or after the plan.
///
/// `planned` substitutes the links the plan would create; without it the same
/// walk states what is there now, which is how a pre-existing arrangement is
/// told apart from one this plan would create.
fn sightings(
    targets: &[InstallTarget],
    probe: &InstallProbe,
    variant: &VariantRef,
    source_dir: &Path,
    planned: bool,
) -> [RootSighting; 3] {
    AgentKind::ALL.map(|agent| {
        let target = &targets[agent.index()];
        let slot = probe.target(agent);
        if matches!(slot.root, RootProbe::Unreadable(_)) {
            return RootSighting::Unread;
        }
        let ours = match &target.disposition {
            TargetDisposition::AlreadyInstalled { .. } => true,
            TargetDisposition::CreateLink | TargetDisposition::CreateRootAndLink => planned,
            _ => false,
        };
        if ours {
            return RootSighting::Offers(SightedEntry::new(
                slot.link_path.clone(),
                source_dir.to_path_buf(),
                Some(variant.clone()),
            ));
        }
        match &slot.content {
            // Content that resolved to no registered variant carries none:
            // compatibility cannot be checked for something Skilled cannot
            // place, and guessing would be the one claim it must not make.
            SlotContent::At(canonical) => RootSighting::Offers(SightedEntry::new(
                slot.link_path.clone(),
                canonical.clone(),
                (canonical == source_dir).then(|| variant.clone()),
            )),
            SlotContent::Nowhere => RootSighting::NothingToLoad,
            SlotContent::Unknown => RootSighting::Unknown,
        }
    })
}

/// The classification alone, so a prediction can be compared with the present
/// without the entries either of them names.
fn predicted_kind(resolution: &OpenCodeResolution) -> u8 {
    match resolution {
        OpenCodeResolution::NothingVisible => 0,
        OpenCodeResolution::Selected { .. } => 1,
        OpenCodeResolution::ForeignExposure { .. } => 2,
        OpenCodeResolution::Conflict { .. } => 3,
        OpenCodeResolution::Incomplete { .. } => 4,
    }
}

fn opencode_concern(resolution: &OpenCodeResolution) -> Option<&'static str> {
    match resolution {
        OpenCodeResolution::ForeignExposure { .. } => {
            Some("can see, but cannot use, the content another agent's root holds")
        }
        OpenCodeResolution::Conflict { .. } => {
            Some("would have more than one directory to choose between")
        }
        _ => None,
    }
}

fn predicted_summary(resolution: &OpenCodeResolution) -> String {
    match resolution {
        OpenCodeResolution::Conflict { entries } => format!(
            "{} of the roots it reads would hold different directories under that name",
            entries.len()
        ),
        OpenCodeResolution::Selected { winner, .. } => format!(
            "it would load the copy in {}'s root instead",
            winner.root().display_name()
        ),
        OpenCodeResolution::ForeignExposure { .. } => {
            "the only definition it can see is another agent's edition".to_owned()
        }
        OpenCodeResolution::Incomplete { roots } => format!(
            "{} of the roots it reads could not be established",
            roots.len()
        ),
        OpenCodeResolution::NothingVisible => "it would find nothing under that name".to_owned(),
    }
}

/// What the user should be told before confirming, that does not stop the work.
fn plan_warnings(sources: &[RegisteredSource], variant: &VariantRef) -> Vec<String> {
    let mut warnings = Vec::new();
    if let Some(source) = sources
        .iter()
        .find(|source| source.id() == variant.source_id())
    {
        // A link points at the working tree, so uncommitted edits are what the
        // agent would load. That is not wrong, but it is not the recorded
        // revision either, and the preview says so rather than implying the
        // install captured a commit.
        if source.dirty() == Some(true) {
            warnings.push(format!(
                "{} has uncommitted changes, so the linked content is its working tree rather \
                 than {}",
                source.label(),
                source.short_head()
            ));
        }
    }
    warnings
}

#[allow(clippy::too_many_arguments)]
fn disposition_for(
    agent: &AgentDetection,
    variant: &VariantRef,
    requested: bool,
    competing: &[VariantRef],
    probe: &TargetProbe,
    source_dir: &Path,
    receipts: &[Receipt],
) -> TargetDisposition {
    // Exclusions are settled before anything is said about the slot: a root
    // the user asked Skilled to leave alone stays unread in the plan as well as
    // on disk, and a target nobody asked for states no finding about whatever
    // happens to be standing in it.
    if !agent.selected() {
        return excluded(ExcludedReason::NotConfigured);
    }
    if !requested {
        return excluded(ExcludedReason::NotRequested);
    }
    if !variant.usable_by(agent.kind()) {
        return excluded(ExcludedReason::Incompatible);
    }
    match narrow(competing, agent.kind()) {
        CandidateSelection::Selected(selected) if &selected == variant => {}
        CandidateSelection::Selected(selected) => {
            return excluded(ExcludedReason::AgentSpecificOverride { selected });
        }
        CandidateSelection::Duplicate(variants) => {
            return TargetDisposition::Blocked {
                finding: duplicate_finding(variant.skill_name(), &variants),
            };
        }
        // `usable_by` already passed and this variant is among the competitors,
        // so it survives its own narrowing. The arm exists because the type
        // admits it, not because a caller can reach it.
        CandidateSelection::NoCandidate => return excluded(ExcludedReason::Incompatible),
    }
    slot_disposition(probe, source_dir, receipts)
}

fn excluded(reason: ExcludedReason) -> TargetDisposition {
    TargetDisposition::Excluded { reason }
}

fn duplicate_finding(skill_name: &str, variants: &[VariantRef]) -> Finding {
    Finding::new(
        "variant.duplicate_for_agent",
        FindingSeverity::Critical,
        format!(
            "{} registered variants answer to {skill_name} for this agent: {}",
            variants.len(),
            variants
                .iter()
                .map(VariantRef::evidence_label)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
}

/// The spec 11.3 target-path contract, applied to one slot.
fn slot_disposition(
    probe: &TargetProbe,
    source_dir: &Path,
    receipts: &[Receipt],
) -> TargetDisposition {
    if let RootProbe::Unreadable(reason) = &probe.root {
        return TargetDisposition::Blocked {
            finding: Finding::new(
                "install.unreadable_root",
                FindingSeverity::Critical,
                format!("the agent's skill root could not be read: {reason}"),
            ),
        };
    }
    match &probe.entry {
        EntryProbe::Absent => match &probe.root {
            RootProbe::Present => TargetDisposition::CreateLink,
            RootProbe::Missing {
                parent_present: true,
            } => TargetDisposition::CreateRootAndLink,
            RootProbe::Missing { .. } => TargetDisposition::Blocked {
                finding: Finding::new(
                    "install.missing_root_parent",
                    FindingSeverity::Critical,
                    format!(
                        "{} does not exist, and Skilled creates only the documented skill root \
                         itself",
                        probe
                            .link_path
                            .parent()
                            .and_then(Path::parent)
                            .unwrap_or(&probe.link_path)
                            .display()
                    ),
                ),
            },
            // Settled above.
            RootProbe::Unreadable(_) => unreachable!("an unreadable root is blocked already"),
        },
        EntryProbe::Symlink {
            canonical: Some(canonical),
            ..
        } if canonical == source_dir => TargetDisposition::AlreadyInstalled {
            managed: receipts
                .iter()
                .any(|receipt| receipt.link_path() == probe.link_path),
        },
        EntryProbe::Symlink { target, canonical } => {
            let managed = receipts
                .iter()
                .any(|receipt| receipt.link_path() == probe.link_path);
            let (code, what) = match (managed, canonical) {
                (true, Some(_)) => (
                    "install.wrong_managed_target",
                    "a link Skilled created points somewhere else".to_owned(),
                ),
                (true, None) => (
                    "install.dangling_symlink",
                    "a link Skilled created no longer resolves".to_owned(),
                ),
                (false, Some(_)) => (
                    "install.unknown_symlink_collision",
                    "a symbolic link Skilled does not own is already there".to_owned(),
                ),
                (false, None) => (
                    "install.dangling_symlink",
                    "a symbolic link that no longer resolves is already there".to_owned(),
                ),
            };
            TargetDisposition::Blocked {
                finding: Finding::new(
                    code,
                    FindingSeverity::Critical,
                    format!(
                        "{what}: it points at {}. Skilled does not repair or replace an existing \
                         entry, so this target is left exactly as it is",
                        target.display()
                    ),
                ),
            }
        }
        EntryProbe::Directory { .. } | EntryProbe::NotADirectory => TargetDisposition::Blocked {
            finding: Finding::new(
                "install.physical_path_collision",
                FindingSeverity::Critical,
                format!(
                    "a {} is already there. Skilled never overwrites one, whatever it holds",
                    if matches!(probe.entry, EntryProbe::Directory { .. }) {
                        "physical directory"
                    } else {
                        "file"
                    }
                ),
            ),
        },
        EntryProbe::Unreadable(reason) => TargetDisposition::Blocked {
            finding: Finding::new(
                "install.unreadable_entry",
                FindingSeverity::Critical,
                format!("something is there that could not be read: {reason}"),
            ),
        },
    }
}

/// Evidence that Skilled created one particular link.
///
/// Spec 7: a receipt is a record of ownership and never an instruction. Nothing
/// in this release recreates a link from one, and the inventory scanner does
/// not consult them at all — they are read here only to tell a link Skilled put
/// down from one it found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Receipt {
    agent: AgentKind,
    skill_name: String,
    link_path: PathBuf,
    link_target: PathBuf,
    source_id: Option<i64>,
    catalog_relative_path: Option<PathBuf>,
    variant_relative_path: Option<PathBuf>,
}

impl Receipt {
    pub(crate) fn new(
        agent: AgentKind,
        skill_name: String,
        link_path: PathBuf,
        link_target: PathBuf,
        source_id: Option<i64>,
        catalog_relative_path: Option<PathBuf>,
        variant_relative_path: Option<PathBuf>,
    ) -> Self {
        Self {
            agent,
            skill_name,
            link_path,
            link_target,
            source_id,
            catalog_relative_path,
            variant_relative_path,
        }
    }

    pub fn agent(&self) -> AgentKind {
        self.agent
    }

    pub fn skill_name(&self) -> &str {
        &self.skill_name
    }

    pub fn link_path(&self) -> &Path {
        &self.link_path
    }

    pub fn link_target(&self) -> &Path {
        &self.link_target
    }

    pub fn source_id(&self) -> Option<i64> {
        self.source_id
    }

    pub fn catalog_relative_path(&self) -> Option<&Path> {
        self.catalog_relative_path.as_deref()
    }

    pub fn variant_relative_path(&self) -> Option<&Path> {
        self.variant_relative_path.as_deref()
    }
}
