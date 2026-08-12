//! Install and repair planning, guarded execution, and verification.
//!
//! Spec 17.2 reserves this module for pure plan builders and guarded executors,
//! and the split is kept literally: [`probe_install`] is the only read of the
//! machine, [`plan_install`] decides everything over the value it returns, and
//! nothing here writes until [`apply_install`] or [`apply_repair`] is called
//! with a plan the user has confirmed.
//!
//! Two rules shape the whole module. A plan blocks whole rather than in part —
//! if any target is blocked, nothing is written anywhere, because spec 15 asks
//! Skilled to stop before writing when it already knows a step would fail. And
//! install never replaces an occupied slot. Repair is a separate single-target
//! pipeline and replaces only a symbolic link whose raw target still matches a
//! Skilled ownership receipt exactly.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::{
    AgentDetection, AgentKind,
    agents::detection_at,
    inventory::{
        Finding, FindingSeverity, InstallationHealth, InstallationObject,
        InstalledSkillObservation, InventoryRow, InventorySnapshot, Provenance, RootStatus,
    },
    resolution::{
        CandidateSelection, OpenCodeResolution, RootSighting, SightedEntry, UnknownCause,
        UnknownRoot, VariantRef, narrow, resolve_opencode, variants_by_name,
    },
    source::{RegisteredSource, SkillValidation, contains_revision},
    store::Store,
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
    /// The slot was not looked at, because the user asked Skilled to leave this
    /// agent alone. Not the same as absent: nothing was observed either way.
    NotRead,
}

/// What the agent's own global skill root holds.
#[derive(Clone, Debug, Eq, PartialEq)]
enum RootProbe {
    Present,
    /// The root does not exist. Whether the plan may create it turns on its
    /// parent: spec 15 has Skilled create the documented root and nothing
    /// above it, so an agent whose own directory is absent is left alone. The
    /// parent is carried rather than re-derived, so the finding that names it
    /// is not doing path arithmetic on an agent convention.
    Missing {
        parent: PathBuf,
        parent_present: bool,
    },
    Unreadable(String),
    /// The root, or a directory on the way to it under the home directory, is
    /// a symbolic link, so writing "into the root" would write somewhere else.
    Redirected {
        via: PathBuf,
    },
    /// The root was not looked at: the user asked Skilled to leave this agent
    /// alone, and [`crate::inventory`] keeps the same rule.
    NotRead,
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
/// rather than a sequence of reads that drifted apart. That moment is earlier
/// than the write, so the target, its root, and the variant directory are each
/// read once more before the link is created; see [`apply_install`]. Nothing
/// else is — the narrowing and the OpenCode prediction stand as the plan the
/// user agreed to, and the scan taken afterwards is what checks them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallProbe {
    source: Result<SourceProbe, String>,
    targets: [TargetProbe; 3],
}

/// The final component of a repair target, preserving why a link could not be
/// resolved. Install only needs to know whether a target resolves; repair must
/// distinguish a genuinely dangling link from one that is merely unreachable.
#[derive(Clone, Debug, Eq, PartialEq)]
enum RepairEntryProbe {
    Absent,
    Symlink {
        target: PathBuf,
        resolution: Result<PathBuf, (io::ErrorKind, String)>,
    },
    Directory,
    NotADirectory,
    Unreadable(String),
    NotRead,
}

/// One read of every filesystem fact a single-target repair depends on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepairProbe {
    target_agent: AgentKind,
    targets: [TargetProbe; 3],
    target_entry: RepairEntryProbe,
    sources: Vec<(VariantRef, Result<SourceProbe, String>)>,
}

impl RepairProbe {
    pub fn target(&self, agent: AgentKind) -> &TargetProbe {
        &self.targets[agent.index()]
    }
}

/// The registered checkout identity and variant directory observed together.
///
/// The revision distinguishes a registered checkout from another repository
/// that later occupies the same pathname. Carrying it beside the canonical
/// checkout and directory lets the immutable plan preserve the identity the
/// preview established for the executor to recheck.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceProbe {
    checkout: PathBuf,
    revision: String,
    directory: PathBuf,
}

impl InstallProbe {
    pub fn target(&self, agent: AgentKind) -> &TargetProbe {
        &self.targets[agent.index()]
    }
}

/// Read the machine once, for one variant.
/// `home` is the directory every documented root hangs off, and is what the
/// walk looking for redirected roots stops at: the home directory itself may
/// legitimately be reached through a link — a macOS temporary directory is —
/// and that is not something Skilled put anything inside of.
pub fn probe_install(
    agents: &[AgentDetection; 3],
    sources: &[RegisteredSource],
    variant: &VariantRef,
    home: &Path,
) -> InstallProbe {
    InstallProbe {
        source: probe_source(sources, variant),
        targets: agents
            .each_ref()
            .map(|agent| probe_target(agent, variant.skill_name(), home)),
    }
}

/// Read the machine once for a repair of one name in one agent's native root.
///
/// All three roots are read even though only one may be written: OpenCode reads
/// the other agents' roots, so predicting its effective resolution requires the
/// same complete sighting set as install planning.
pub fn probe_repair(
    agents: &[AgentDetection; 3],
    sources: &[RegisteredSource],
    skill_name: &str,
    agent: AgentKind,
    home: &Path,
) -> RepairProbe {
    let targets = agents
        .each_ref()
        .map(|detection| probe_target(detection, skill_name, home));
    let target_entry = if detection_at(agents, agent).selected() {
        probe_repair_entry(&targets[agent.index()].link_path)
    } else {
        RepairEntryProbe::NotRead
    };
    let sources = variants_by_name(sources)
        .remove(skill_name)
        .unwrap_or_default()
        .into_iter()
        .map(|variant| {
            let source = probe_source(sources, &variant);
            (variant, source)
        })
        .collect();
    RepairProbe {
        target_agent: agent,
        targets,
        target_entry,
        sources,
    }
}

fn probe_repair_entry(link_path: &Path) -> RepairEntryProbe {
    let file_type = match fs::symlink_metadata(link_path) {
        Ok(metadata) => metadata.file_type(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return RepairEntryProbe::Absent,
        Err(error) => return RepairEntryProbe::Unreadable(error.to_string()),
    };
    if file_type.is_symlink() {
        let target = match fs::read_link(link_path) {
            Ok(target) => target,
            Err(error) => return RepairEntryProbe::Unreadable(error.to_string()),
        };
        let resolution = link_path
            .canonicalize()
            .map_err(|error| (error.kind(), error.to_string()));
        return RepairEntryProbe::Symlink { target, resolution };
    }
    if file_type.is_dir() {
        RepairEntryProbe::Directory
    } else {
        RepairEntryProbe::NotADirectory
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
fn probe_source(sources: &[RegisteredSource], variant: &VariantRef) -> Result<SourceProbe, String> {
    let source = sources
        .iter()
        .find(|source| source.id() == variant.source_id())
        .ok_or_else(|| "its source is no longer registered".to_owned())?;
    let checkout = verified_checkout(source.git_top_level(), source.head())?;
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
    // A registered path that has since become a file is not a skill directory,
    // and a preview that offered to link to it would promise work that cannot
    // happen. The apply guard checks this too; catching it here is what keeps
    // the preview and the outcome the same statement.
    if !fs::metadata(&directory).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(format!("{} is no longer a directory", directory.display()));
    }
    let mut budget = InspectionBudget::source_scan();
    validate_portable_skill_with_budget(&directory, &mut budget)
        .map_err(|error| format!("it no longer validates as a portable skill: {error}"))?;
    Ok(SourceProbe {
        checkout,
        revision: source.head().to_owned(),
        directory,
    })
}

/// One agent's slot, read only if that agent is one Skilled was asked to
/// manage.
///
/// Selection is checked before anything is touched, exactly as
/// [`crate::inventory::scan_installations`] checks it: a root the user asked
/// Skilled to leave alone stays unread, so nothing in it can decide anything —
/// not this agent's own target, and not what the plan says about OpenCode.
fn probe_target(agent: &AgentDetection, skill_name: &str, home: &Path) -> TargetProbe {
    let link_path = agent.root().join(skill_name);
    if !agent.selected() {
        return TargetProbe {
            agent: agent.kind(),
            link_path,
            root: RootProbe::NotRead,
            entry: EntryProbe::NotRead,
            content: SlotContent::Unknown,
        };
    }
    let entry = probe_entry(&link_path);
    TargetProbe {
        root: probe_root(agent.root(), home),
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
        EntryProbe::Unreadable(_) | EntryProbe::NotRead => return SlotContent::Unknown,
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

fn probe_root(root: &Path, home: &Path) -> RootProbe {
    // Before anything else: the path the preview states has to be the path the
    // write lands on. `fs::metadata` follows links, so a root — or any
    // directory on the way to it below the home directory — that is a symbolic
    // link would present as an ordinary directory while the link Skilled
    // created went somewhere the user was never shown. Skilled writes only
    // inside a documented root it established, so a redirected one is refused
    // rather than followed; adopting a redirected layout is a decision for a
    // release that can also repair one.
    if let Some(via) = redirected_component(root, home) {
        return RootProbe::Redirected { via };
    }
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
            let parent = root.parent().unwrap_or(root).to_path_buf();
            RootProbe::Missing {
                parent_present: fs::metadata(&parent).is_ok_and(|meta| meta.is_dir()),
                parent,
            }
        }
        Err(error) => RootProbe::Unreadable(error.to_string()),
    }
}

/// The first directory between `home` and `root`, inclusive of `root`, that is
/// a symbolic link.
///
/// The walk stops at the home directory rather than canonicalizing the whole
/// path, because a home directory reached through a link is ordinary — every
/// macOS temporary directory is one — and only the components Skilled would
/// write through are its business.
fn redirected_component(root: &Path, home: &Path) -> Option<PathBuf> {
    let mut component = Some(root);
    while let Some(path) = component {
        if path == home {
            return None;
        }
        if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Some(path.to_path_buf());
        }
        component = path.parent();
    }
    None
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
    /// to write. `receipted` says whether Skilled holds a receipt for this
    /// path — which is not quite the same as having created the link that is
    /// there now, and is worded as what it is wherever it is reported. A link
    /// Skilled has no receipt for stays unowned: adopting one is a later
    /// slice's decision to make.
    AlreadyInstalled { receipted: bool },
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

/// What the plan expects OpenCode to resolve the name to once it has run.
///
/// Carried so the check afterwards has something to check *against*. Spec 11.4
/// asks whether the machine ended up the way the plan said it would, which is
/// a different question from whether the arrangement is one Skilled would have
/// chosen: an install that knowingly leaves OpenCode ambiguous says so before
/// it runs, and a user who confirmed that has not been surprised by it.
/// The winner is carried, not only the classification. Two arrangements can
/// both be "one directory selected" and be different directories: another
/// root's link appearing after the plan was made would leave OpenCode loading
/// content the plan never described, and a check that compared only the word
/// would call that a match.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpenCodeOutlook {
    /// OpenCode will load the directory reached through this installation slot.
    Selected { winner: PathBuf },
    /// The only definition it can see will be one Skilled cannot claim is
    /// usable by OpenCode, at this slot.
    Exposure { winner: PathBuf },
    /// More than one directory will answer to the name.
    Conflict,
    /// Nothing will.
    Nothing,
    /// A root Skilled did not read leaves it unknowable.
    Unknown,
}

impl OpenCodeOutlook {
    fn of(resolution: &OpenCodeResolution) -> Self {
        match resolution {
            OpenCodeResolution::Selected { winner, .. } => Self::Selected {
                winner: winner.path().to_path_buf(),
            },
            OpenCodeResolution::ForeignExposure { winner, .. }
            | OpenCodeResolution::IncompatibleExposure { winner, .. } => Self::Exposure {
                winner: winner.path().to_path_buf(),
            },
            OpenCodeResolution::Conflict { .. } => Self::Conflict,
            OpenCodeResolution::NothingVisible => Self::Nothing,
            OpenCodeResolution::Incomplete { .. } => Self::Unknown,
        }
    }

    /// A consent-surface account of what OpenCode is expected to do after the
    /// plan. Paths remain absolute here and are terminal-escaped by the caller.
    pub fn preview_summary(&self) -> String {
        match self {
            Self::Selected { winner } => format!("load {}", winner.display()),
            Self::Exposure { winner } => format!(
                "see {} without a variant Skilled can claim is usable",
                winner.display()
            ),
            Self::Conflict => "conflict".to_owned(),
            Self::Nothing => "load nothing under this name".to_owned(),
            Self::Unknown => "could not be established".to_owned(),
        }
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
    source_checkout: PathBuf,
    source_revision: String,
    source_dir: PathBuf,
    targets: Vec<InstallTarget>,
    warnings: Vec<String>,
    opencode_outlook: Option<OpenCodeOutlook>,
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

    /// What this plan expects OpenCode to resolve the name to afterwards, or
    /// `None` where OpenCode is not an agent Skilled was asked to manage.
    pub fn opencode_outlook(&self) -> Option<&OpenCodeOutlook> {
        self.opencode_outlook.as_ref()
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

/// What a repair plan will do with its one target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RepairDisposition {
    /// Replace the proven link. `dangling` says whether the old target was
    /// absent, as distinct from resolving to the wrong registered variant.
    ReplaceLink { dangling: bool },
    /// The link already resolves to the variant the registry selects today.
    NothingToRepair,
    /// A precondition is not proven. A blocked plan is inert.
    Blocked { finding: Finding },
}

/// One immutable, single-target repair statement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepairPlan {
    agent: AgentKind,
    skill_name: String,
    link_path: PathBuf,
    recorded_target: PathBuf,
    current_target: PathBuf,
    variant: Option<VariantRef>,
    source_checkout: Option<PathBuf>,
    source_revision: Option<String>,
    source_dir: Option<PathBuf>,
    old_source_label: Option<String>,
    new_source_label: Option<String>,
    source_changed: bool,
    disposition: RepairDisposition,
    warnings: Vec<String>,
    opencode_outlook: Option<OpenCodeOutlook>,
}

impl RepairPlan {
    pub fn agent(&self) -> AgentKind {
        self.agent
    }

    pub fn skill_name(&self) -> &str {
        &self.skill_name
    }

    pub fn link_path(&self) -> &Path {
        &self.link_path
    }

    /// The old target as an absolute path for consent surfaces.
    pub fn current_target(&self) -> &Path {
        &self.current_target
    }

    /// The exact spelling recorded in the link and receipt, for the apply
    /// guard. It may differ from `current_target` only for legacy relative
    /// receipts.
    fn recorded_target(&self) -> &Path {
        &self.recorded_target
    }

    pub fn new_target(&self) -> Option<&Path> {
        self.source_dir.as_deref()
    }

    pub fn variant(&self) -> Option<&VariantRef> {
        self.variant.as_ref()
    }

    pub fn old_source_label(&self) -> Option<&str> {
        self.old_source_label.as_deref()
    }

    pub fn new_source_label(&self) -> Option<&str> {
        self.new_source_label.as_deref()
    }

    pub fn source_changed(&self) -> bool {
        self.source_changed
    }

    pub fn disposition(&self) -> &RepairDisposition {
        &self.disposition
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn opencode_outlook(&self) -> Option<&OpenCodeOutlook> {
        self.opencode_outlook.as_ref()
    }

    pub fn blocking_finding(&self) -> Option<&Finding> {
        match &self.disposition {
            RepairDisposition::Blocked { finding } => Some(finding),
            _ => None,
        }
    }

    pub fn is_executable(&self) -> bool {
        matches!(self.disposition, RepairDisposition::ReplaceLink { .. })
    }
}

/// The newest receipt that proves ownership of the observed link.
///
/// A path-only receipt is insufficient: receipts outlive links, so a third
/// party can remove and recreate one at the same path. Byte-identical target
/// spelling is the evidence that the object still names what Skilled recorded.
pub fn receipt_for<'a>(
    receipts: &'a [Receipt],
    agent: AgentKind,
    link_path: &Path,
    observed_target: &Path,
) -> Option<&'a Receipt> {
    receipts.iter().rev().find(|receipt| {
        receipt.agent() == agent
            && receipt.link_path() == link_path
            && receipt.link_target() == observed_target
    })
}

/// Decide what repairing one observed link would do.
///
/// Pure over the probe, registry, and receipts. Every refusal is represented in
/// the returned inert plan so preview and scripted surfaces can state the exact
/// path and evidence without touching the filesystem.
pub fn plan_repair(
    _agents: &[AgentDetection; 3],
    sources: &[RegisteredSource],
    skill_name: &str,
    agent: AgentKind,
    probe: &RepairProbe,
    receipts: &[Receipt],
) -> RepairPlan {
    debug_assert_eq!(probe.target_agent, agent);
    let target = probe.target(agent);
    let mut plan = empty_repair_plan(agent, skill_name, target.link_path.clone());

    if let Some(finding) = repair_root_finding(&target.root) {
        plan.disposition = RepairDisposition::Blocked { finding };
        return plan;
    }

    let (observed_target, current_resolution) = match &probe.target_entry {
        RepairEntryProbe::Absent => {
            plan.disposition = blocked_repair(
                "repair.nothing_to_replace",
                "there is no symbolic link at this path to replace",
            );
            return plan;
        }
        RepairEntryProbe::Directory | RepairEntryProbe::NotADirectory => {
            plan.disposition = blocked_repair(
                "install.physical_path_collision",
                "a physical file or directory occupies this path; Skilled never overwrites one",
            );
            return plan;
        }
        RepairEntryProbe::Unreadable(reason) => {
            plan.disposition = blocked_repair(
                "install.unreadable_entry",
                format!("the entry could not be read: {reason}"),
            );
            return plan;
        }
        RepairEntryProbe::NotRead => {
            plan.disposition = blocked_repair(
                "install.unreadable_root",
                "Skilled did not read this agent's skill root",
            );
            return plan;
        }
        RepairEntryProbe::Symlink {
            target: observed,
            resolution,
        } => {
            if matches!(resolution, Err((kind, _)) if *kind != io::ErrorKind::NotFound) {
                let reason = match resolution {
                    Err((_, reason)) => reason,
                    Ok(_) => unreachable!(),
                };
                plan.recorded_target = observed.clone();
                plan.current_target = absolute_link_target(&target.link_path, observed);
                plan.disposition = blocked_repair(
                    "install.unresolvable_symlink",
                    format!("the symbolic link could not be resolved: {reason}"),
                );
                return plan;
            }
            (observed.clone(), resolution.clone())
        }
    };
    plan.recorded_target = observed_target.clone();
    plan.current_target = absolute_link_target(&target.link_path, &observed_target);

    let Some(receipt) = receipt_for(receipts, agent, &target.link_path, &observed_target) else {
        plan.disposition = blocked_repair(
            "repair.unproven_link",
            "Skilled holds no receipt matching the symbolic link that is there, so it cannot prove it created this object",
        );
        return plan;
    };
    plan.old_source_label = receipt.source_id().and_then(|source_id| {
        sources
            .iter()
            .find(|source| source.id() == source_id)
            .map(|source| source.label().to_owned())
    });

    if repair_registry_incomplete(sources, receipt.source_id()) {
        plan.disposition = blocked_repair(
            "repair.registry_incomplete",
            "a registered source or included catalog that could change the selected target could not be read",
        );
        return plan;
    }

    let competing = variants_by_name(sources)
        .remove(receipt.skill_name())
        .unwrap_or_default();
    let variant = match narrow(&competing, agent) {
        CandidateSelection::Selected(variant) => variant,
        CandidateSelection::Duplicate(variants) => {
            plan.disposition = RepairDisposition::Blocked {
                finding: duplicate_finding(receipt.skill_name(), &variants),
            };
            return plan;
        }
        CandidateSelection::NoCandidate => {
            plan.disposition = blocked_repair(
                "repair.variant_unavailable",
                format!(
                    "no registered source currently offers a usable variant named {} for this agent",
                    receipt.skill_name()
                ),
            );
            return plan;
        }
    };
    let source = match probe
        .sources
        .iter()
        .find(|(candidate, _)| candidate == &variant)
        .map(|(_, source)| source)
    {
        Some(Ok(source)) => source,
        Some(Err(reason)) => {
            plan.disposition = blocked_repair(
                "repair.variant_unavailable",
                format!("the selected variant is not currently usable: {reason}"),
            );
            return plan;
        }
        None => {
            plan.disposition = blocked_repair(
                "repair.variant_unavailable",
                "the selected variant was not present when the filesystem was probed",
            );
            return plan;
        }
    };

    plan.skill_name = receipt.skill_name().to_owned();
    plan.new_source_label = Some(variant.source_label().to_owned());
    plan.source_changed = receipt
        .source_id()
        .is_some_and(|source_id| source_id != variant.source_id());
    plan.source_checkout = Some(source.checkout.clone());
    plan.source_revision = Some(source.revision.clone());
    plan.source_dir = Some(source.directory.clone());
    plan.variant = Some(variant.clone());
    plan.warnings = plan_warnings(sources, &variant);
    plan.disposition = match current_resolution {
        Ok(current) if current == source.directory => RepairDisposition::NothingToRepair,
        Ok(_) => RepairDisposition::ReplaceLink { dangling: false },
        Err((io::ErrorKind::NotFound, _)) => RepairDisposition::ReplaceLink { dangling: true },
        Err(_) => unreachable!("non-NotFound resolution failures returned above"),
    };

    let predicted = resolve_opencode(repair_sightings(
        probe,
        agent,
        &variant,
        &source.directory,
        true,
    ));
    let current = resolve_opencode(repair_sightings(
        probe,
        agent,
        &variant,
        &source.directory,
        false,
    ));
    plan.opencode_outlook = Some(OpenCodeOutlook::of(&predicted));
    if agent == AgentKind::OpenCode
        && plan.is_executable()
        && matches!(predicted, OpenCodeResolution::Conflict { .. })
    {
        plan.disposition = blocked_repair(
            "install.opencode_conflict",
            format!(
                "OpenCode would not resolve {} to this link: the roots it reads would hold more than one directory under that name",
                plan.skill_name
            ),
        );
    } else if let OpenCodeResolution::Incomplete { roots } = &predicted {
        plan.warnings.push(format!(
            "what OpenCode would resolve {} to cannot be established: {}",
            plan.skill_name,
            unknown_roots(roots)
        ));
    } else if plan.is_executable()
        && predicted_kind(&predicted) != predicted_kind(&current)
        && let Some(concern) = opencode_concern(&predicted)
    {
        plan.warnings.push(format!(
            "after this repair, OpenCode {concern} for {}",
            plan.skill_name
        ));
    }
    plan
}

fn empty_repair_plan(agent: AgentKind, skill_name: &str, link_path: PathBuf) -> RepairPlan {
    RepairPlan {
        agent,
        skill_name: skill_name.to_owned(),
        link_path,
        recorded_target: PathBuf::new(),
        current_target: PathBuf::new(),
        variant: None,
        source_checkout: None,
        source_revision: None,
        source_dir: None,
        old_source_label: None,
        new_source_label: None,
        source_changed: false,
        disposition: RepairDisposition::Blocked {
            finding: Finding::new(
                "repair.variant_unavailable",
                FindingSeverity::Critical,
                "no repair target was established".to_owned(),
            ),
        },
        warnings: Vec::new(),
        opencode_outlook: None,
    }
}

fn blocked_repair(code: &'static str, evidence: impl Into<String>) -> RepairDisposition {
    RepairDisposition::Blocked {
        finding: Finding::new(code, FindingSeverity::Critical, evidence.into()),
    }
}

fn repair_root_finding(root: &RootProbe) -> Option<Finding> {
    match root {
        RootProbe::Present => None,
        RootProbe::Missing { .. } => Some(Finding::new(
            "repair.nothing_to_replace",
            FindingSeverity::Critical,
            "the agent's skill root is absent, so there is no symbolic link to replace".to_owned(),
        )),
        RootProbe::Unreadable(reason) => Some(Finding::new(
            "install.unreadable_root",
            FindingSeverity::Critical,
            format!("the agent's skill root could not be read: {reason}"),
        )),
        RootProbe::Redirected { via } => Some(Finding::new(
            "install.redirected_root",
            FindingSeverity::Critical,
            format!(
                "{} is a symbolic link, so replacing a link below it could write somewhere other than the path shown",
                via.display()
            ),
        )),
        RootProbe::NotRead => Some(Finding::new(
            "install.unreadable_root",
            FindingSeverity::Critical,
            "Skilled did not read this agent's skill root".to_owned(),
        )),
    }
}

fn absolute_link_target(link_path: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() {
        target.to_path_buf()
    } else {
        link_path.parent().unwrap_or(link_path).join(target)
    }
}

fn repair_registry_incomplete(sources: &[RegisteredSource], own_source_id: Option<i64>) -> bool {
    sources.iter().any(|source| {
        if source.source_error().is_some() {
            return Some(source.id()) != own_source_id;
        }
        source
            .catalogs()
            .iter()
            .any(|catalog| catalog.included() && catalog.scan_error().is_some())
    })
}

fn repair_sightings(
    probe: &RepairProbe,
    repaired_agent: AgentKind,
    variant: &VariantRef,
    source_dir: &Path,
    planned: bool,
) -> [RootSighting; 3] {
    AgentKind::ALL.map(|agent| {
        let slot = probe.target(agent);
        if matches!(
            slot.root,
            RootProbe::Unreadable(_) | RootProbe::NotRead | RootProbe::Redirected { .. }
        ) {
            return RootSighting::Unread;
        }
        if agent == repaired_agent && planned {
            return RootSighting::Offers(SightedEntry::new(
                slot.link_path.clone(),
                source_dir.to_path_buf(),
                Some(variant.clone()),
            ));
        }
        match &slot.content {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RepairOfferStatus {
    Offered,
    NotOffered { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RepairOffer {
    path: PathBuf,
    status: RepairOfferStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OverlayFinding {
    path: PathBuf,
    finding: Finding,
}

impl OverlayFinding {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn finding(&self) -> &Finding {
        &self.finding
    }
}

/// Receipt-aware facts held beside, never inside, the receipt-blind inventory.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RepairOverlay {
    offers: Vec<RepairOffer>,
    findings: Vec<OverlayFinding>,
    receipts_error: Option<String>,
}

impl RepairOverlay {
    pub fn build(
        snapshot: &InventorySnapshot,
        receipts: &[Receipt],
        sources: &[RegisteredSource],
        _agents: &[AgentDetection; 3],
    ) -> Self {
        let by_name = variants_by_name(sources);
        let mut overlay = Self::default();
        for row in snapshot.rows() {
            for observation in row.observations() {
                let InstallationObject::Symlink { target } = observation.object() else {
                    continue;
                };
                let matching =
                    receipt_for(receipts, observation.agent(), observation.path(), target);
                let status = match matching {
                    None => RepairOfferStatus::NotOffered {
                        reason: "Skilled holds no receipt matching the link that is there"
                            .to_owned(),
                    },
                    Some(_receipt)
                        if observation.findings().iter().any(|finding| {
                            finding.code() == "install.unresolvable_symlink"
                        }) => RepairOfferStatus::NotOffered {
                            reason: "the link is unreachable for a reason other than absence"
                                .to_owned(),
                        },
                    Some(receipt) if repair_registry_incomplete(sources, receipt.source_id()) => {
                        RepairOfferStatus::NotOffered {
                            reason: "the registry is incomplete, so the correct target cannot be selected safely"
                                .to_owned(),
                        }
                    }
                    Some(receipt) => match by_name
                        .get(receipt.skill_name())
                        .map_or(CandidateSelection::NoCandidate, |variants| {
                            narrow(variants, observation.agent())
                        }) {
                        CandidateSelection::Selected(_) => RepairOfferStatus::Offered,
                        CandidateSelection::Duplicate(_) => RepairOfferStatus::NotOffered {
                            reason: "more than one registered variant answers for this agent"
                                .to_owned(),
                        },
                        CandidateSelection::NoCandidate => RepairOfferStatus::NotOffered {
                            reason: "no usable registered variant is available for this agent"
                                .to_owned(),
                        },
                    },
                };

                if matches!(status, RepairOfferStatus::Offered)
                    && observation.findings().is_empty()
                    && let Some(current) = observation.resolution()
                    && let Some(CandidateSelection::Selected(selected)) = by_name
                        .get(row.name())
                        .map(|variants| narrow(variants, observation.agent()))
                    && (selected.source_id(), selected.variant_relative_path())
                        != (current.source_id(), current.variant_relative_path())
                {
                    overlay.findings.push(OverlayFinding {
                        path: observation.path().to_path_buf(),
                        finding: Finding::new(
                            "install.wrong_managed_target",
                            FindingSeverity::Warning,
                            format!(
                                "this link resolves to {}, but the registry now selects {} for this agent",
                                current.evidence_label(),
                                selected.evidence_label()
                            ),
                        ),
                    });
                }
                overlay.offers.push(RepairOffer {
                    path: observation.path().to_path_buf(),
                    status,
                });
            }
        }
        overlay
    }

    pub fn receipts_unread(reason: String) -> Self {
        Self {
            receipts_error: Some(reason),
            ..Self::default()
        }
    }

    pub fn offer(&self, path: &Path) -> RepairOfferStatus {
        if let Some(reason) = &self.receipts_error {
            return RepairOfferStatus::NotOffered {
                reason: format!("the ownership receipts could not be read: {reason}"),
            };
        }
        self.offers
            .iter()
            .find(|offer| offer.path == path)
            .map_or_else(
                || RepairOfferStatus::NotOffered {
                    reason: "this finding does not concern a repairable symbolic link".to_owned(),
                },
                |offer| offer.status.clone(),
            )
    }

    pub fn is_offered(&self, path: &Path) -> bool {
        matches!(self.offer(path), RepairOfferStatus::Offered)
    }

    pub(crate) fn findings(&self) -> &[OverlayFinding] {
        &self.findings
    }
    pub fn finding_at(&self, path: &Path) -> Option<&Finding> {
        self.findings
            .iter()
            .find(|finding| finding.path == path)
            .map(|finding| &finding.finding)
    }
    pub fn finding_count(&self) -> usize {
        self.findings.len()
    }
    pub fn receipts_readable(&self) -> bool {
        self.receipts_error.is_none()
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
    // The variant has to still be one the registry offers under this name. A
    // caller can hold a `VariantRef` from a row it focused some time ago, and a
    // variant whose source became unreadable, or that stopped validating, is
    // no longer something an agent would resolve to. This is checked against
    // the registry as the caller holds it, which is what every other decision
    // in this module is made over; the filesystem is read once, in the probe.
    if !competing.contains(variant) {
        return Err(PlanFailure::VariantUnavailable {
            skill_name: variant.skill_name().to_owned(),
        });
    }
    let source = probe
        .source
        .as_ref()
        .map_err(|reason| PlanFailure::SourceUnavailable {
            reason: reason.clone(),
        })?;
    let source_dir = source.directory.clone();

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
    let opencode_outlook = detection_at(agents, AgentKind::OpenCode)
        .selected()
        .then(|| {
            apply_opencode_prediction(&mut targets, probe, variant, &source_dir, &mut warnings)
        });

    Ok(InstallPlan {
        variant: variant.clone(),
        source_checkout: source.checkout.clone(),
        source_revision: source.revision.clone(),
        source_dir,
        targets,
        warnings,
        opencode_outlook,
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
///
/// A resolution that could not be established is never a refusal either. A root
/// Skilled was told to leave alone might hold anything, and a plan that blocked
/// on that would punish the user for a choice they made deliberately; the
/// prediction says so and the postcondition check afterwards withholds a verdict
/// over the very same gap, so the two rest on one account of what was read.
///
/// A conflict that already exists is left to Doctor, which is where a standing
/// arrangement belongs: only a change this plan would make is worth saying here.
fn apply_opencode_prediction(
    targets: &mut [InstallTarget],
    probe: &InstallProbe,
    variant: &VariantRef,
    source_dir: &Path,
    warnings: &mut Vec<String>,
) -> OpenCodeOutlook {
    let index = AgentKind::OpenCode.index();
    if matches!(
        targets[index].disposition,
        TargetDisposition::Blocked { .. }
    ) {
        // Its own finding is the more direct account of the same slot, and a
        // target nothing will be written to has no outlook to state.
        return OpenCodeOutlook::Unknown;
    }
    let predicted = resolve_opencode(sightings(targets, probe, variant, source_dir, true));
    let outlook = OpenCodeOutlook::of(&predicted);
    if targets[index].is_work() {
        if matches!(predicted, OpenCodeResolution::Conflict { .. }) {
            targets[index].disposition = TargetDisposition::Blocked {
                finding: Finding::new(
                    "install.opencode_conflict",
                    FindingSeverity::Critical,
                    format!(
                        "OpenCode would not resolve {} to this link: the roots it reads would \
                         hold more than one directory under that name",
                        variant.skill_name()
                    ),
                ),
            };
            return outlook;
        }
        if let OpenCodeResolution::Incomplete { roots } = &predicted {
            warnings.push(format!(
                "what OpenCode would resolve {} to cannot be established: {}",
                variant.skill_name(),
                unknown_roots(roots)
            ));
        }
        return outlook;
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
    outlook
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
        // A root that was not read in full contributes no sighting at all — not
        // an absence — which is the rule `inventory::root_sightings` applies to
        // the same three roots. Both have to apply it, because the prediction
        // made here and the postcondition checked against a later scan are
        // statements about one arrangement.
        if matches!(
            slot.root,
            RootProbe::Unreadable(_) | RootProbe::NotRead | RootProbe::Redirected { .. }
        ) {
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
        OpenCodeResolution::IncompatibleExposure { .. } => 3,
        OpenCodeResolution::Conflict { .. } => 4,
        OpenCodeResolution::Incomplete { .. } => 5,
    }
}

fn opencode_concern(resolution: &OpenCodeResolution) -> Option<&'static str> {
    match resolution {
        OpenCodeResolution::ForeignExposure { .. } => {
            Some("can see, but cannot use, the content another agent's root holds")
        }
        OpenCodeResolution::IncompatibleExposure { .. } => {
            Some("can see a registered definition whose catalog is not registered for OpenCode")
        }
        OpenCodeResolution::Conflict { .. } => {
            Some("would have more than one directory to choose between")
        }
        _ => None,
    }
}

/// What a resolution taken after the apply has to say, for a verification
/// failure's evidence.
fn observed_summary(resolution: &OpenCodeResolution) -> String {
    match resolution {
        OpenCodeResolution::Conflict { .. } => {
            "the roots it reads hold more than one directory under that name".to_owned()
        }
        OpenCodeResolution::Selected { winner, .. } => format!(
            "it loads the copy in {}'s root instead",
            winner.root().display_name()
        ),
        OpenCodeResolution::ForeignExposure { .. } => {
            "the only definition it can see is another agent's edition".to_owned()
        }
        OpenCodeResolution::IncompatibleExposure { .. } => {
            "the only registered definition it can see comes from a catalog not registered for \
             OpenCode"
                .to_owned()
        }
        OpenCodeResolution::Incomplete { roots } => unknown_roots(roots),
        OpenCodeResolution::NothingVisible => "it finds nothing under that name".to_owned(),
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

/// Whether Skilled holds an ownership receipt for this exact path.
///
/// Deliberately not the same claim as "Skilled created the object that is there
/// now". A receipt outlives the link it describes — that is what makes it
/// evidence for a later repair — so a link removed and remade by hand at the
/// same path still matches one. Skilled records no inode or creation time, so
/// it cannot tell those apart, and every surface that reports this says what it
/// actually knows: that a receipt exists for the path.
fn receipted(receipts: &[Receipt], link_path: &Path) -> bool {
    receipts
        .iter()
        .any(|receipt| receipt.link_path() == link_path)
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
    match &probe.root {
        RootProbe::Unreadable(reason) => {
            return TargetDisposition::Blocked {
                finding: Finding::new(
                    "install.unreadable_root",
                    FindingSeverity::Critical,
                    format!("the agent's skill root could not be read: {reason}"),
                ),
            };
        }
        // Every caller settles a deselected agent as an exclusion before
        // reaching this, so an unread root here would be a plan built from
        // nothing. Refusing is the only honest answer to it.
        RootProbe::NotRead => {
            return TargetDisposition::Blocked {
                finding: Finding::new(
                    "install.unreadable_root",
                    FindingSeverity::Critical,
                    "Skilled did not read this agent's skill root, so it can say nothing about \
                     what is in it"
                        .to_owned(),
                ),
            };
        }
        RootProbe::Redirected { via } => {
            return TargetDisposition::Blocked {
                finding: Finding::new(
                    "install.redirected_root",
                    FindingSeverity::Critical,
                    format!(
                        "{} is a symbolic link, so a link written under this path would land \
                         somewhere other than the path shown. Skilled writes only inside a root \
                         it established",
                        via.display()
                    ),
                ),
            };
        }
        RootProbe::Present | RootProbe::Missing { .. } => {}
    }
    match &probe.entry {
        EntryProbe::Absent => match &probe.root {
            RootProbe::Present => TargetDisposition::CreateLink,
            RootProbe::Missing {
                parent_present: true,
                ..
            } => TargetDisposition::CreateRootAndLink,
            RootProbe::Missing { parent, .. } => TargetDisposition::Blocked {
                finding: Finding::new(
                    "install.missing_root_parent",
                    FindingSeverity::Critical,
                    format!(
                        "{} does not exist, and Skilled creates only the documented skill root \
                         itself",
                        parent.display()
                    ),
                ),
            },
            // Settled above.
            RootProbe::Unreadable(_) | RootProbe::NotRead | RootProbe::Redirected { .. } => {
                unreachable!("an unread, unreadable, or redirected root is blocked already")
            }
        },
        EntryProbe::Symlink {
            canonical: Some(canonical),
            ..
        } if canonical == source_dir => TargetDisposition::AlreadyInstalled {
            receipted: receipted(receipts, &probe.link_path),
        },
        EntryProbe::Symlink { target, canonical } => {
            let receipted = receipted(receipts, &probe.link_path);
            let (code, what) = match (receipted, canonical) {
                (true, Some(_)) => (
                    "install.wrong_managed_target",
                    "Skilled holds a receipt for this path, and what is there now points \
                     somewhere else"
                        .to_owned(),
                ),
                (true, None) => (
                    "install.dangling_symlink",
                    "Skilled holds a receipt for this path, and what is there now no longer \
                     resolves"
                        .to_owned(),
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
        // Settled above, with the root it belongs to.
        EntryProbe::NotRead => unreachable!("an unread root is blocked already"),
    }
}

/// What happened to one target the plan called work.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StepOutcome {
    /// The link was created and its ownership receipt recorded.
    Created,
    /// The link was created but the receipt could not be written, so Skilled
    /// does not own something it put on disk. Stated rather than hidden: the
    /// link is real, and a later repair will not recognise it.
    CreatedUnrecorded(String),
    /// The skill root was created, but the link could not be. The empty root is
    /// deliberately left in place, so this is a partial write rather than a
    /// failed step that changed nothing.
    RootCreatedLinkFailed(String),
    /// Nothing was written to this target, and why.
    Failed(String),
    /// The run stopped before reaching this target.
    Unattempted,
}

/// One target, and what the apply did about it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppliedStep {
    agent: AgentKind,
    link_path: PathBuf,
    outcome: StepOutcome,
}

impl AppliedStep {
    pub fn agent(&self) -> AgentKind {
        self.agent
    }

    pub fn link_path(&self) -> &Path {
        &self.link_path
    }

    pub fn outcome(&self) -> &StepOutcome {
        &self.outcome
    }

    fn wrote_link(&self) -> bool {
        matches!(
            self.outcome,
            StepOutcome::Created | StepOutcome::CreatedUnrecorded(_)
        )
    }

    pub(crate) fn changed_filesystem(&self) -> bool {
        matches!(
            self.outcome,
            StepOutcome::Created
                | StepOutcome::CreatedUnrecorded(_)
                | StepOutcome::RootCreatedLinkFailed(_)
        )
    }
}

/// Everything one apply did, step by step (spec 19).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ApplyReport {
    steps: Vec<AppliedStep>,
}

impl ApplyReport {
    pub fn steps(&self) -> &[AppliedStep] {
        &self.steps
    }

    pub fn step(&self, agent: AgentKind) -> Option<&AppliedStep> {
        self.steps.iter().find(|step| step.agent == agent)
    }
}

/// One postcondition the fresh scan did not bear out.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifyFailure {
    agent: AgentKind,
    observed: String,
}

impl VerifyFailure {
    pub fn agent(&self) -> AgentKind {
        self.agent
    }

    /// What the scan taken after the apply actually found.
    pub fn observed(&self) -> &str {
        &self.observed
    }
}

/// One postcondition the fresh scan could not settle either way.
///
/// Kept apart from a failure for the reason [`crate::inventory`] keeps the same
/// two apart: a check that found the wrong thing and a check that could not run
/// are different answers, and calling the second of them a pass would let
/// Skilled report a postcondition it never observed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifyWithheld {
    agent: AgentKind,
    reason: String,
    /// Whether this is a postcondition on a target Skilled wrote, as opposed
    /// to an ancillary OpenCode resolution affected through another root.
    required: bool,
}

impl VerifyWithheld {
    pub fn agent(&self) -> AgentKind {
        self.agent
    }

    /// What stopped the check, in words.
    pub fn reason(&self) -> &str {
        &self.reason
    }

    fn blocks_success(&self) -> bool {
        self.required
    }
}

/// What a scan taken after the apply made of the links it wrote.
///
/// Spec 11.4: exit status zero is not sufficient by itself, so every created
/// link is re-observed and checked against what the plan said it would be.
///
/// Three answers, never two. [`Self::is_verified`] means every written target
/// was re-observed and nothing failed, which is not the same as every ancillary
/// postcondition holding: an effective-resolution check Skilled could not run
/// is carried separately so the surfaces can say so without turning a root the
/// user deselected into a failed install.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VerifyReport {
    failures: Vec<VerifyFailure>,
    withheld: Vec<VerifyWithheld>,
}

impl VerifyReport {
    /// Whether every written target was checked and nothing disagreed with the
    /// plan. Ancillary OpenCode resolution may remain withheld without turning
    /// an otherwise observed install into a failure.
    pub fn is_verified(&self) -> bool {
        self.failures.is_empty() && !self.withheld.iter().any(VerifyWithheld::blocks_success)
    }

    /// Whether every postcondition was both checked and held.
    pub fn is_complete(&self) -> bool {
        self.failures.is_empty() && self.withheld.is_empty()
    }

    pub fn failures(&self) -> &[VerifyFailure] {
        &self.failures
    }

    pub fn withheld(&self) -> &[VerifyWithheld] {
        &self.withheld
    }
}

/// The single word an install run ends on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallStatus {
    /// The plan held no work, so nothing was written and nothing failed.
    NothingToDo,
    /// Every planned link was created, every written target was observed again,
    /// and nothing disagreed with the plan. Whether every ancillary
    /// postcondition was checked is [`VerifyReport::is_complete`]; a status is
    /// one word, and this is not the place to flatten the two answers into it.
    Installed,
    /// Some links were created and the run stopped before the rest.
    PartiallyApplied,
    /// The run stopped before writing anything at all.
    NotApplied,
    /// Everything was written, and the scan afterwards either did not bear it
    /// out or could not re-observe a written target.
    VerificationFailed,
    /// Every link was created, and at least one receipt could not be written.
    /// Skilled has put something on disk that it does not own, which a later
    /// repair or uninstall will not recognise as its own.
    ///
    /// Only the last work target can reach this: a receipt that cannot be
    /// written means the metadata store is failing, so the run stops there and
    /// anything behind it is reported as unattempted.
    InstalledUnrecorded,
}

/// One completed install run: what was planned, what was done, and what the
/// scan afterwards made of it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallOutcome {
    plan: InstallPlan,
    applied: ApplyReport,
    verification: VerifyReport,
}

impl InstallOutcome {
    pub(crate) fn new(plan: InstallPlan, applied: ApplyReport, verification: VerifyReport) -> Self {
        Self {
            plan,
            applied,
            verification,
        }
    }

    pub fn plan(&self) -> &InstallPlan {
        &self.plan
    }

    pub fn applied(&self) -> &ApplyReport {
        &self.applied
    }

    pub fn verification(&self) -> &VerifyReport {
        &self.verification
    }

    pub fn step(&self, agent: AgentKind) -> Option<&AppliedStep> {
        self.applied.step(agent)
    }

    /// The verdict, settled in the order a reader would want it.
    ///
    /// What was written is decided before what the scan made of it: a run that
    /// stopped part way through is partially applied whatever the links it did
    /// manage to write turned out to be, and calling it a verification failure
    /// would name the smaller of the two problems.
    pub fn status(&self) -> InstallStatus {
        install_status(&self.applied, &self.verification)
    }
}

fn install_status(applied: &ApplyReport, verification: &VerifyReport) -> InstallStatus {
    if applied.steps.is_empty() {
        return InstallStatus::NothingToDo;
    }
    if !applied.steps.iter().all(AppliedStep::wrote_link) {
        return if applied.steps.iter().any(AppliedStep::changed_filesystem) {
            InstallStatus::PartiallyApplied
        } else {
            InstallStatus::NotApplied
        };
    }
    // Ownership is settled before the postcondition. A link Skilled cannot
    // record owning is the more consequential of the two: verification can
    // be run again, and a receipt that was never written is gone.
    if applied
        .steps
        .iter()
        .any(|step| matches!(step.outcome, StepOutcome::CreatedUnrecorded(_)))
    {
        return InstallStatus::InstalledUnrecorded;
    }
    if !verification.is_verified() {
        return InstallStatus::VerificationFailed;
    }
    InstallStatus::Installed
}

/// What the install flow is showing, and therefore what it will accept.
///
/// Held by [`crate::SkilledApp`] and rendered as a modal dialog. The two states
/// are not interchangeable: a preview is a question, and only a preview accepts
/// a confirmation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstallPrompt {
    /// What would happen, awaiting the user's answer.
    Preview(InstallPlan),
    /// What did happen.
    Report(InstallOutcome),
    /// No plan could be made at all, and why.
    Failed(String),
}

/// Create every link the plan calls work, in [`AgentKind::ALL`] order.
///
/// Spec 15 is applied literally. The target, its root, and the variant
/// directory the link would point at are all read again immediately before the
/// write, because the preview the user confirmed described an earlier moment;
/// anything that changed stops that target, and the run stops there rather than
/// carrying on into the targets behind it. Nothing is ever replaced, so a link
/// that appeared in the meantime is a failed precondition and never an
/// unlink-and-retry.
///
/// A plan that blocks blocks whole, and this refuses one rather than trusting
/// its callers to: a plan is either executable or it is not written at all.
///
/// One window is left open and is recorded rather than papered over. Every
/// guard here resolves a path by name, so a process that replaced the checked
/// root — or a directory above it — between the check and the `symlink` call
/// would have the link created somewhere other than the path the plan stated.
/// Closing it needs `openat` and `symlinkat` against a directory handle, which
/// the standard library does not expose; that is a production dependency, and
/// this project takes those under explicit review rather than at a closeout.
/// Tracked as `skilled-cb2`.
///
/// It is not silent if it happens. The scan that follows reads the documented
/// root, finds nothing at the path the plan named, and [`verify_install`]
/// reports that failure — so the outcome is a run that says it did not end up
/// where it said it would, rather than one that claims success. An attacker
/// able to reach the window already has write access to the user's home
/// directory.
///
/// There is no rollback. The links written before a failure are real, healthy,
/// receipted installations, and deleting them would be a second unrequested
/// write on top of an operation that already went wrong. Spec 19 permits
/// same-operation rollback and does not require it; the report says exactly
/// what exists.
pub(crate) fn apply_install(plan: &InstallPlan, store: &Store, home: &Path) -> ApplyReport {
    let mut steps: Vec<AppliedStep> = Vec::new();
    if plan.is_blocked() {
        // Both callers refuse a blocked plan before reaching this, so arriving
        // here is a bug rather than a state to report. A debug build says so;
        // a release build still writes nothing.
        debug_assert!(false, "a blocked plan reached apply_install");
        return ApplyReport { steps };
    }
    let mut stopped = false;
    for target in plan.targets().iter().filter(|target| target.is_work()) {
        if stopped {
            steps.push(AppliedStep {
                agent: target.agent,
                link_path: target.link_path.clone(),
                outcome: StepOutcome::Unattempted,
            });
            continue;
        }
        let outcome = apply_target(plan, target, store, home);
        stopped = !matches!(outcome, StepOutcome::Created);
        steps.push(AppliedStep {
            agent: target.agent,
            link_path: target.link_path.clone(),
            outcome,
        });
    }
    ApplyReport { steps }
}

fn apply_target(
    plan: &InstallPlan,
    target: &InstallTarget,
    store: &Store,
    home: &Path,
) -> StepOutcome {
    let root = match target.link_path.parent() {
        Some(root) => root,
        None => return StepOutcome::Failed("the target has no parent directory".to_owned()),
    };
    // The guards, not conveniences: what the plan described and what is there
    // now must still agree, or this target is left exactly as it is.
    if probe_entry(&target.link_path) != EntryProbe::Absent {
        return StepOutcome::Failed(
            "something arrived at this path after the plan was shown, so nothing was written to it"
                .to_owned(),
        );
    }
    // The variant directory is checked too. A checkout moved or removed between
    // the preview and the confirmation would otherwise leave Skilled owning a
    // link it created that resolves to nothing; install still refuses every
    // occupied slot, while repair is a separate explicitly confirmed action.
    match plan.source_dir().canonicalize() {
        Ok(resolved)
            if resolved == plan.source_dir()
                && fs::metadata(&resolved).is_ok_and(|metadata| metadata.is_dir()) => {}
        _ => {
            return StepOutcome::Failed(format!(
                "{} is no longer the directory the plan resolved, so nothing was written",
                plan.source_dir().display()
            ));
        }
    }
    let mut budget = InspectionBudget::source_scan();
    if let Err(error) = validate_portable_skill_with_budget(plan.source_dir(), &mut budget) {
        return StepOutcome::Failed(format!(
            "{} no longer validates as a portable skill, so nothing was written: {error}",
            plan.source_dir().display()
        ));
    }
    let receipt = Receipt {
        operation: ReceiptOperation::Install,
        agent: target.agent,
        skill_name: plan.skill_name().to_owned(),
        link_path: target.link_path.clone(),
        link_target: plan.source_dir().to_path_buf(),
        source_id: Some(plan.variant.source_id()),
        catalog_relative_path: Some(plan.variant.catalog_relative_path().to_path_buf()),
        variant_relative_path: Some(plan.variant.variant_relative_path().to_path_buf()),
    };
    if let Err(error) = store.ensure_receipt_recordable(&receipt) {
        return StepOutcome::Failed(format!(
            "the ownership receipt cannot record this target, so nothing was written: {error}"
        ));
    }
    if let Err(reason) = recheck_source_identity(plan) {
        return StepOutcome::Failed(format!("{reason}, so nothing was written"));
    }
    let root_now = probe_root(root, home);
    let root_created = match (&target.disposition, &root_now) {
        (TargetDisposition::CreateLink, RootProbe::Present) => false,
        (
            TargetDisposition::CreateRootAndLink,
            RootProbe::Missing {
                parent_present: true,
                ..
            },
        ) => {
            // One level, never recursive: the plan named this directory and
            // only this directory.
            if let Err(error) = fs::create_dir(root) {
                return StepOutcome::Failed(format!(
                    "the skill root could not be created: {error}"
                ));
            }
            true
        }
        _ => {
            return StepOutcome::Failed(
                "the agent's skill root changed after the plan was shown, so nothing was written \
                 to it"
                    .to_owned(),
            );
        }
    };
    if let Err(error) = create_directory_symlink(plan.source_dir(), &target.link_path) {
        return if root_created {
            // The empty documented root is left in place. Removing it would be
            // an unrequested second write after the operation already failed.
            StepOutcome::RootCreatedLinkFailed(error.to_string())
        } else {
            StepOutcome::Failed(format!("the link could not be created: {error}"))
        };
    }
    match store.record_receipt(&receipt) {
        Ok(()) => StepOutcome::Created,
        Err(error) => StepOutcome::CreatedUnrecorded(error.to_string()),
    }
}

/// Re-establish that the link target belongs to the checkout the plan named.
///
/// Canonical directory equality alone cannot distinguish a repository that
/// replaced the registered checkout at the same path. The registered revision
/// is the store's existing identity witness: if the current repository no
/// longer contains it, this is another checkout and no write may follow.
fn recheck_source_identity(plan: &InstallPlan) -> Result<(), String> {
    verified_checkout(&plan.source_checkout, &plan.source_revision).map(|_| ())
}

fn verified_checkout(expected: &Path, revision: &str) -> Result<PathBuf, String> {
    let checkout = expected
        .canonicalize()
        .map_err(|error| format!("the source checkout identity could not be verified: {error}"))?;
    if checkout != expected {
        return Err(format!(
            "the source now resolves to a different Git checkout: {}",
            checkout.display()
        ));
    }
    match contains_revision(&checkout, revision) {
        Ok(true) => Ok(checkout),
        Ok(false) => Err("the source path now contains a different Git checkout".to_owned()),
        Err(error) => Err(format!(
            "the source checkout identity could not be verified: {error}"
        )),
    }
}

#[cfg(unix)]
fn create_directory_symlink(target: &Path, link_path: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link_path)
}

#[cfg(windows)]
fn create_directory_symlink(target: &Path, link_path: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link_path)
}

/// A platform whose directory symbolic links Skilled has no adapter for.
///
/// Refusing to write is the only honest answer: the whole installation shape is
/// a directory link, and there is nothing else this release would put in its
/// place. The plan still builds and still previews, so a reader is told what
/// would happen rather than met with a build that does not exist.
#[cfg(not(any(unix, windows)))]
fn create_directory_symlink(_target: &Path, _link_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Skilled installs skills as directory symbolic links, which this platform does not offer",
    ))
}

/// What happened when the single repair target was applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RepairStepOutcome {
    Repaired,
    RepairedUnrecorded(String),
    /// Windows removed the proven old link before creating its replacement,
    /// and creation then failed. Skilled did not install a replacement; the
    /// path may be absent or occupied by an object that raced with creation.
    RemovedUnreplaced(String),
    /// Unix could not remove the sibling temporary link after its atomic rename
    /// failed. The original installation remains, but this additional link is a
    /// residual write that must be reported with its exact path.
    ResidualTemporary {
        path: PathBuf,
        error: String,
    },
    Failed(String),
}

impl RepairStepOutcome {
    fn wrote_link(&self) -> bool {
        matches!(self, Self::Repaired | Self::RepairedUnrecorded(_))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepairAppliedStep {
    agent: AgentKind,
    link_path: PathBuf,
    outcome: RepairStepOutcome,
}

impl RepairAppliedStep {
    pub fn agent(&self) -> AgentKind {
        self.agent
    }

    pub fn link_path(&self) -> &Path {
        &self.link_path
    }

    pub fn outcome(&self) -> &RepairStepOutcome {
        &self.outcome
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RepairApplyReport {
    step: Option<RepairAppliedStep>,
}

impl RepairApplyReport {
    pub fn step(&self) -> Option<&RepairAppliedStep> {
        self.step.as_ref()
    }
}

/// Replace the one proven link after re-establishing every preview guard.
///
/// Unix creates a sibling temporary link and renames it over the destination.
/// `rename(2)` is atomic within one directory, so readers never observe the
/// installation path absent. A pathname race remains after the last guard:
/// unlike install's fail-if-exists race, rename can unlink a file or stranger's
/// link that arrives in that window. Closing it requires handle-relative APIs
/// the standard library does not expose; this data-loss window is tracked as a
/// follow-up and is stated here rather than hidden.
///
/// Windows cannot atomically rename over an existing directory symlink. Its
/// fallback removes the directory link with `remove_dir` and creates the new
/// one, and is therefore explicitly non-atomic. A creation failure after the
/// removal is a partial apply and is reported as such rather than as an inert
/// refusal.
pub(crate) fn apply_repair(plan: &RepairPlan, store: &Store, home: &Path) -> RepairApplyReport {
    if !plan.is_executable() {
        return RepairApplyReport::default();
    }
    let outcome = apply_repair_target(plan, store, home);
    RepairApplyReport {
        step: Some(RepairAppliedStep {
            agent: plan.agent,
            link_path: plan.link_path.clone(),
            outcome,
        }),
    }
}

fn apply_repair_target(plan: &RepairPlan, store: &Store, home: &Path) -> RepairStepOutcome {
    let Some(root) = plan.link_path.parent() else {
        return RepairStepOutcome::Failed("the target has no parent directory".to_owned());
    };
    if let Err(reason) = repair_destination_unchanged(plan, root, home) {
        return RepairStepOutcome::Failed(reason);
    }
    let (Some(source_dir), Some(source_checkout), Some(source_revision), Some(variant)) = (
        plan.source_dir.as_deref(),
        plan.source_checkout.as_deref(),
        plan.source_revision.as_deref(),
        plan.variant.as_ref(),
    ) else {
        return RepairStepOutcome::Failed("the plan carries no repair source".to_owned());
    };
    match source_dir.canonicalize() {
        Ok(resolved)
            if resolved == source_dir
                && fs::metadata(&resolved).is_ok_and(|metadata| metadata.is_dir()) => {}
        _ => {
            return RepairStepOutcome::Failed(format!(
                "{} is no longer the directory the plan resolved, so nothing was written",
                source_dir.display()
            ));
        }
    }
    let mut budget = InspectionBudget::source_scan();
    if let Err(error) = validate_portable_skill_with_budget(source_dir, &mut budget) {
        return RepairStepOutcome::Failed(format!(
            "{} no longer validates as a portable skill, so nothing was written: {error}",
            source_dir.display()
        ));
    }
    let receipt = Receipt {
        operation: ReceiptOperation::Repair,
        agent: plan.agent,
        skill_name: plan.skill_name.clone(),
        link_path: plan.link_path.clone(),
        link_target: source_dir.to_path_buf(),
        source_id: Some(variant.source_id()),
        catalog_relative_path: Some(variant.catalog_relative_path().to_path_buf()),
        variant_relative_path: Some(variant.variant_relative_path().to_path_buf()),
    };
    if let Err(error) = store.ensure_receipt_recordable(&receipt) {
        return RepairStepOutcome::Failed(format!(
            "the ownership receipt cannot record this target, so nothing was written: {error}"
        ));
    }
    if let Err(reason) = verified_checkout(source_checkout, source_revision) {
        return RepairStepOutcome::Failed(format!("{reason}, so nothing was written"));
    }
    // Source validation, receipt checks, and Git identity verification can all
    // take time. Re-establish the destination guards at the mutation boundary
    // so that only the final syscall window tracked by skilled-2k3.6.1 remains.
    if let Err(reason) = repair_destination_unchanged(plan, root, home) {
        return RepairStepOutcome::Failed(reason);
    }
    if let Err(error) = replace_directory_symlink(source_dir, &plan.link_path) {
        return match error {
            ReplaceLinkError::Unchanged(error) => {
                RepairStepOutcome::Failed(format!("the link could not be replaced: {error}"))
            }
            #[cfg(windows)]
            ReplaceLinkError::RemovedOldLink(error) => {
                RepairStepOutcome::RemovedUnreplaced(error.to_string())
            }
            #[cfg(unix)]
            ReplaceLinkError::ResidualTemporary {
                path,
                rename_error,
                cleanup_error,
            } => RepairStepOutcome::ResidualTemporary {
                path,
                error: format!(
                    "the replacement rename failed ({rename_error}), then cleanup failed: {cleanup_error}"
                ),
            },
        };
    }
    match store.record_receipt(&receipt) {
        Ok(()) => RepairStepOutcome::Repaired,
        Err(error) => RepairStepOutcome::RepairedUnrecorded(error.to_string()),
    }
}

fn repair_destination_unchanged(plan: &RepairPlan, root: &Path, home: &Path) -> Result<(), String> {
    if probe_root(root, home) != RootProbe::Present {
        return Err(
            "the agent's skill root changed after the plan was shown, so nothing was written"
                .to_owned(),
        );
    }
    match probe_repair_entry(&plan.link_path) {
        RepairEntryProbe::Symlink { target, .. } if target == plan.recorded_target() => Ok(()),
        _ => Err("the entry changed after the plan was shown, so nothing was written".to_owned()),
    }
}

#[cfg(unix)]
fn replace_directory_symlink(target: &Path, link_path: &Path) -> Result<(), ReplaceLinkError> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let parent = link_path.parent().ok_or_else(|| {
        ReplaceLinkError::Unchanged(io::Error::new(
            io::ErrorKind::InvalidInput,
            "target has no parent",
        ))
    })?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(".skilled-repair-{}-{nonce}", std::process::id()));
    create_directory_symlink(target, &temporary).map_err(ReplaceLinkError::Unchanged)?;
    if let Err(error) = fs::rename(&temporary, link_path) {
        return match fs::remove_file(&temporary) {
            Ok(()) => Err(ReplaceLinkError::Unchanged(error)),
            Err(cleanup_error) => Err(ReplaceLinkError::ResidualTemporary {
                path: temporary,
                rename_error: error,
                cleanup_error,
            }),
        };
    }
    Ok(())
}

#[cfg(windows)]
fn replace_directory_symlink(target: &Path, link_path: &Path) -> Result<(), ReplaceLinkError> {
    fs::remove_dir(link_path).map_err(ReplaceLinkError::Unchanged)?;
    create_directory_symlink(target, link_path).map_err(ReplaceLinkError::RemovedOldLink)
}

#[cfg(not(any(unix, windows)))]
fn replace_directory_symlink(_target: &Path, _link_path: &Path) -> Result<(), ReplaceLinkError> {
    Err(ReplaceLinkError::Unchanged(io::Error::new(
        io::ErrorKind::Unsupported,
        "this platform does not support Skilled's directory-link repair",
    )))
}

#[derive(Debug)]
enum ReplaceLinkError {
    /// The destination still names the same object it did before this attempt.
    Unchanged(io::Error),
    /// The proven old link was removed before replacement creation failed.
    #[cfg(windows)]
    RemovedOldLink(io::Error),
    /// The replacement rename failed and its sibling temporary link could not
    /// be removed, leaving an additional filesystem entry behind.
    #[cfg(unix)]
    ResidualTemporary {
        path: PathBuf,
        rename_error: io::Error,
        cleanup_error: io::Error,
    },
}

/// Check a repaired link and its OpenCode outlook against a fresh scan.
pub fn verify_repair(
    plan: &RepairPlan,
    applied: &RepairApplyReport,
    snapshot: &InventorySnapshot,
) -> VerifyReport {
    let mut failures = Vec::new();
    let mut withheld = Vec::new();
    let Some(step) = applied.step.as_ref() else {
        return VerifyReport { failures, withheld };
    };
    if !step.outcome.wrote_link() {
        let reason = match &step.outcome {
            RepairStepOutcome::RemovedUnreplaced(_) => {
                "the original link was removed but no replacement was written, so there was no repaired link to verify"
            }
            RepairStepOutcome::ResidualTemporary { path, .. } => {
                withheld.push(VerifyWithheld {
                    agent: step.agent,
                    reason: format!(
                        "the failed repair left a temporary link at {}",
                        path.display()
                    ),
                    required: true,
                });
                return VerifyReport { failures, withheld };
            }
            _ => "the repair was not applied, so there was no repaired link to verify",
        };
        withheld.push(VerifyWithheld {
            agent: step.agent,
            reason: reason.to_owned(),
            required: true,
        });
        return VerifyReport { failures, withheld };
    }
    if let Some(reason) = unscanned(snapshot.root(step.agent).status()) {
        withheld.push(VerifyWithheld {
            agent: step.agent,
            reason,
            required: true,
        });
        return VerifyReport { failures, withheld };
    }
    let row = snapshot.row(plan.skill_name());
    let Some(observed) = row.and_then(|row| row.observation(step.agent)) else {
        failures.push(VerifyFailure {
            agent: step.agent,
            observed: "the scan taken afterwards found nothing at this path".to_owned(),
        });
        return VerifyReport { failures, withheld };
    };
    let Some(variant) = plan.variant() else {
        failures.push(VerifyFailure {
            agent: step.agent,
            observed: "the repair plan carried no registered variant identity".to_owned(),
        });
        return VerifyReport { failures, withheld };
    };
    let Some(expected_target) = plan.new_target() else {
        failures.push(VerifyFailure {
            agent: step.agent,
            observed: "the repair plan carried no target path".to_owned(),
        });
        return VerifyReport { failures, withheld };
    };
    match observed.object() {
        InstallationObject::Symlink { target } if target == expected_target => {}
        InstallationObject::Symlink { target } => {
            failures.push(VerifyFailure {
                agent: step.agent,
                observed: format!(
                    "the symbolic link records {} instead of the planned target {}",
                    target.display(),
                    expected_target.display()
                ),
            });
            return VerifyReport { failures, withheld };
        }
        _ => {}
    }
    match mismatch_variant(variant, observed) {
        Checked::Held => {}
        Checked::Failed(observed) => failures.push(VerifyFailure {
            agent: step.agent,
            observed,
        }),
        Checked::Withheld(reason) => withheld.push(VerifyWithheld {
            agent: step.agent,
            reason,
            required: true,
        }),
    }
    if failures.is_empty() && !withheld.iter().any(VerifyWithheld::blocks_success) {
        match row.and_then(InventoryRow::opencode_resolution) {
            Some(OpenCodeResolution::Incomplete { roots }) => withheld.push(VerifyWithheld {
                agent: AgentKind::OpenCode,
                reason: format!(
                    "what OpenCode resolves the name to could not be established: {}",
                    unknown_roots(roots)
                ),
                required: false,
            }),
            Some(resolution) => {
                let actual = OpenCodeOutlook::of(resolution);
                if plan
                    .opencode_outlook()
                    .is_some_and(|expected| expected != &actual)
                {
                    failures.push(VerifyFailure {
                        agent: AgentKind::OpenCode,
                        observed: format!(
                            "this was not what the plan described: {}",
                            observed_summary(resolution)
                        ),
                    });
                }
            }
            None if plan.opencode_outlook().is_some() => withheld.push(VerifyWithheld {
                agent: AgentKind::OpenCode,
                reason: "the fresh scan produced no OpenCode resolution, so that ancillary postcondition was not checked"
                    .to_owned(),
                required: false,
            }),
            None => {}
        }
    }
    VerifyReport { failures, withheld }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepairStatus {
    NothingToRepair,
    Repaired,
    NotApplied,
    PartiallyApplied,
    RepairedUnrecorded,
    VerificationFailed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepairOutcome {
    plan: RepairPlan,
    applied: RepairApplyReport,
    verification: VerifyReport,
}

impl RepairOutcome {
    pub(crate) fn new(
        plan: RepairPlan,
        applied: RepairApplyReport,
        verification: VerifyReport,
    ) -> Self {
        Self {
            plan,
            applied,
            verification,
        }
    }

    pub fn plan(&self) -> &RepairPlan {
        &self.plan
    }
    pub fn applied(&self) -> &RepairApplyReport {
        &self.applied
    }
    pub fn verification(&self) -> &VerifyReport {
        &self.verification
    }

    pub fn status(&self) -> RepairStatus {
        match self.applied.step.as_ref().map(|step| &step.outcome) {
            None if matches!(self.plan.disposition, RepairDisposition::NothingToRepair) => {
                RepairStatus::NothingToRepair
            }
            None => RepairStatus::NotApplied,
            Some(RepairStepOutcome::Failed(_)) => RepairStatus::NotApplied,
            Some(RepairStepOutcome::RemovedUnreplaced(_)) => RepairStatus::PartiallyApplied,
            Some(RepairStepOutcome::ResidualTemporary { .. }) => RepairStatus::PartiallyApplied,
            Some(RepairStepOutcome::RepairedUnrecorded(_)) => RepairStatus::RepairedUnrecorded,
            Some(RepairStepOutcome::Repaired) if !self.verification.is_verified() => {
                RepairStatus::VerificationFailed
            }
            Some(RepairStepOutcome::Repaired) => RepairStatus::Repaired,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RepairPrompt {
    Preview(RepairPlan),
    Report(RepairOutcome),
    Failed(String),
}

/// Check every link the apply wrote against the plan that called for it.
///
/// Pure, over a scan taken after the apply. The observation has to be a
/// symbolic link resolving to a registered variant with this identity, valid
/// and healthy — and where OpenCode was written to, its effective resolution
/// has to be the link that was just written rather than something reached
/// through a compatibility root.
///
/// OpenCode is checked even when it was not a target. It reads Claude Code's
/// and Codex's roots, so a link written into either is one it now sees, and an
/// ambiguity that write created belongs in this report rather than only in
/// Doctor.
///
/// An effective resolution the scan could not establish is not a failed
/// postcondition. A root the user deselected is one Skilled never read, and
/// calling the gap a failure would report an install as unverified for doing
/// exactly what the user configured. The planner withholds its refusal over the
/// same gap, so the two agree about what was and was not observed.
pub fn verify_install(
    plan: &InstallPlan,
    applied: &ApplyReport,
    snapshot: &InventorySnapshot,
) -> VerifyReport {
    let mut failures = Vec::new();
    let mut withheld = Vec::new();
    let mut opencode_native = false;
    let row = snapshot.row(plan.skill_name());
    for step in applied.steps.iter().filter(|step| step.wrote_link()) {
        // A root the scan could not read says nothing about the link in it.
        // The scan is bounded and can exhaust its budget over a large registry,
        // and a root can become unreadable between the write and the rescan;
        // reporting either as a failed postcondition would call a correct
        // install broken for a reason that has nothing to do with it.
        if let Some(reason) = unscanned(snapshot.root(step.agent).status()) {
            withheld.push(VerifyWithheld {
                agent: step.agent,
                reason,
                required: true,
            });
            continue;
        }
        let Some(observed) = row.and_then(|row| row.observation(step.agent)) else {
            failures.push(VerifyFailure {
                agent: step.agent,
                observed: "the scan taken afterwards found nothing at this path".to_owned(),
            });
            continue;
        };
        match mismatch(plan, observed) {
            Checked::Held => {}
            Checked::Failed(observed) => {
                failures.push(VerifyFailure {
                    agent: step.agent,
                    observed,
                });
                continue;
            }
            Checked::Withheld(reason) => {
                withheld.push(VerifyWithheld {
                    agent: step.agent,
                    reason,
                    required: true,
                });
                continue;
            }
        }
        if step.agent != AgentKind::OpenCode {
            continue;
        }
        opencode_native = true;
        let Some(resolution) = row.and_then(InventoryRow::opencode_resolution) else {
            continue;
        };
        if matches!(
            resolution,
            OpenCodeResolution::Selected { winner, .. } if winner.path() == step.link_path
        ) {
            continue;
        }
        if let OpenCodeResolution::Incomplete { roots } = resolution {
            withheld.push(VerifyWithheld {
                agent: step.agent,
                reason: format!(
                    "what OpenCode resolves the name to could not be established: {}",
                    unknown_roots(roots)
                ),
                required: false,
            });
            continue;
        }
        failures.push(VerifyFailure {
            agent: step.agent,
            observed: format!(
                "OpenCode does not resolve the name to this link: {}",
                observed_summary(resolution)
            ),
        });
    }

    // A link written into Claude Code's or Codex's root is one OpenCode reads
    // too, so what OpenCode now loads is a postcondition of that write even
    // though OpenCode was not a target. Only an ambiguity is a failure: an
    // install that leaves OpenCode loading another agent's copy of a common
    // skill, or seeing an edition it cannot use, is the arrangement the plan
    // described and warned about, not a broken one.
    if !opencode_native
        && applied.steps.iter().any(AppliedStep::wrote_link)
        && let Some(observed) = row.and_then(InventoryRow::opencode_resolution)
    {
        // Checked against what the plan said, not against what Skilled would
        // have preferred. An install that knowingly leaves OpenCode ambiguous
        // says so before it runs and is confirmed with that in front of the
        // reader; a conflict that has been there all along is Doctor's. What
        // this catches is the arrangement changing into something the plan did
        // not describe, which is the whole reason a postcondition is checked.
        match (plan.opencode_outlook(), OpenCodeOutlook::of(observed)) {
            (_, OpenCodeOutlook::Unknown) => {
                let OpenCodeResolution::Incomplete { roots } = observed else {
                    unreachable!("only an incomplete resolution is unknown")
                };
                withheld.push(VerifyWithheld {
                    agent: AgentKind::OpenCode,
                    reason: format!(
                        "what OpenCode resolves the name to could not be established: {}",
                        unknown_roots(roots)
                    ),
                    required: false,
                });
            }
            (Some(expected), actual) if expected != &actual => failures.push(VerifyFailure {
                agent: AgentKind::OpenCode,
                observed: format!(
                    "this was not what the plan described: {}",
                    observed_summary(observed)
                ),
            }),
            _ => {}
        }
    }
    VerifyReport { failures, withheld }
}

/// Name each root whose contribution is unknown, and say which kind of unknown
/// it is.
///
/// [`crate::resolution`] keeps a root it never read apart from one it read in
/// full whose entry it could not follow, and refuses to flatten them. Anything
/// that reports them has to keep them apart too: "Skilled did not read this"
/// is simply false of the second.
fn unknown_roots(roots: &[UnknownRoot]) -> String {
    roots
        .iter()
        .map(|root| {
            let name = root.root().display_name();
            match root.cause() {
                UnknownCause::RootNotRead => format!("Skilled did not read {name}'s skill root"),
                UnknownCause::EntryUnresolved => {
                    format!("what {name}'s skill root holds under that name could not be followed")
                }
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Why a root contributed nothing to the scan, where that is not the scan
/// having read it and found nothing.
fn unscanned(status: &RootStatus) -> Option<String> {
    match status {
        RootStatus::Scanned { .. } | RootStatus::Missing => None,
        RootStatus::NotSelected => {
            Some("the scan taken afterwards was not asked to read this root".to_owned())
        }
        RootStatus::NotScanned => Some("this root has not been read since".to_owned()),
        RootStatus::Unreadable { message } => Some(format!(
            "the scan taken afterwards could not read this root: {message}"
        )),
    }
}

/// What one postcondition came to.
///
/// Three answers, because a check that found the wrong thing and a check that
/// could not be made are not the same answer, and treating the second as a pass
/// would report a postcondition Skilled never observed.
enum Checked {
    Held,
    Failed(String),
    Withheld(String),
}

/// Compare the fresh observation against the plan that called for it.
fn mismatch(plan: &InstallPlan, observed: &InstalledSkillObservation) -> Checked {
    mismatch_variant(plan.variant(), observed)
}

fn mismatch_variant(variant: &VariantRef, observed: &InstalledSkillObservation) -> Checked {
    let InstallationObject::Symlink { .. } = observed.object() else {
        return Checked::Failed(format!(
            "what is there is {}, not a symbolic link",
            observed.object().description()
        ));
    };
    match observed.resolution() {
        Some(resolution) if resolution == variant => {}
        Some(resolution) => {
            return Checked::Failed(format!(
                "it resolves to {} instead",
                resolution.evidence_label()
            ));
        }
        // A source that could not be read leaves provenance unestablished, not
        // established as "from nowhere". The scan says which of the two it is,
        // and this keeps them apart.
        None if matches!(observed.provenance(), Provenance::Unverified) => {
            return Checked::Withheld(
                "a registered source could not be read, so where this came from is not \
                 established"
                    .to_owned(),
            );
        }
        None => {
            return Checked::Failed(format!(
                "it does not resolve to a registered variant: {}",
                observed.provenance().label()
            ));
        }
    }
    if !observed.validation().is_some_and(SkillValidation::is_valid) {
        return Checked::Failed(
            observed
                .validation()
                .and_then(SkillValidation::message)
                .map_or_else(
                    || "the installed content could not be validated".to_owned(),
                    |message| format!("the installed content does not validate: {message}"),
                ),
        );
    }
    match observed.health() {
        InstallationHealth::Healthy => Checked::Held,
        // Structurally sound content whose provenance could not be established
        // is the same gap as above, reached by the roll-up rather than by the
        // resolution.
        InstallationHealth::Unverified => Checked::Withheld(
            "a registered source could not be read, so this installation is unverified".to_owned(),
        ),
        health => Checked::Failed(format!(
            "the scan taken afterwards calls it {}",
            health.label()
        )),
    }
}

/// Which guarded operation created the link described by a receipt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiptOperation {
    Install,
    Repair,
}

impl ReceiptOperation {
    pub(crate) fn identifier(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Repair => "repair",
        }
    }

    pub(crate) fn from_identifier(identifier: &str) -> crate::Result<Self> {
        match identifier {
            "install" => Ok(Self::Install),
            "repair" => Ok(Self::Repair),
            other => Err(crate::Error::InvalidStoredReceiptOperation(
                other.to_owned(),
            )),
        }
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
    operation: ReceiptOperation,
    agent: AgentKind,
    skill_name: String,
    link_path: PathBuf,
    link_target: PathBuf,
    source_id: Option<i64>,
    catalog_relative_path: Option<PathBuf>,
    variant_relative_path: Option<PathBuf>,
}

impl Receipt {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        operation: ReceiptOperation,
        agent: AgentKind,
        skill_name: String,
        link_path: PathBuf,
        link_target: PathBuf,
        source_id: Option<i64>,
        catalog_relative_path: Option<PathBuf>,
        variant_relative_path: Option<PathBuf>,
    ) -> Self {
        Self {
            operation,
            agent,
            skill_name,
            link_path,
            link_target,
            source_id,
            catalog_relative_path,
            variant_relative_path,
        }
    }

    pub fn operation(&self) -> ReceiptOperation {
        self.operation
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Creating only the root is still a filesystem mutation. It is not a
    /// written link for verification, but it makes a failed step a partial
    /// apply rather than a truthful `NotApplied` outcome.
    #[test]
    fn a_root_created_before_link_failure_is_a_partial_write() {
        let applied = ApplyReport {
            steps: vec![AppliedStep {
                agent: AgentKind::ClaudeCode,
                link_path: PathBuf::from("/home/example/.claude/skills/portable"),
                outcome: StepOutcome::RootCreatedLinkFailed("permission denied".to_owned()),
            }],
        };
        let verification = VerifyReport::default();

        assert_eq!(
            install_status(&applied, &verification),
            InstallStatus::PartiallyApplied
        );
        assert!(applied.steps[0].changed_filesystem());
        assert!(!applied.steps[0].wrote_link());
    }

    /// Windows must remove a directory symlink before it can create its
    /// replacement. If creation then fails, that is a partial mutation rather
    /// than the same `NotApplied` state as a guard refusal.
    #[test]
    fn an_old_link_removed_before_replacement_failure_is_a_partial_repair() {
        let mut plan = empty_repair_plan(
            AgentKind::Codex,
            "portable",
            PathBuf::from("/home/example/.agents/skills/portable"),
        );
        plan.disposition = RepairDisposition::ReplaceLink { dangling: false };
        let applied = RepairApplyReport {
            step: Some(RepairAppliedStep {
                agent: AgentKind::Codex,
                link_path: plan.link_path.clone(),
                outcome: RepairStepOutcome::RemovedUnreplaced("permission denied".to_owned()),
            }),
        };
        let outcome = RepairOutcome::new(plan, applied, VerifyReport::default());

        assert_eq!(outcome.status(), RepairStatus::PartiallyApplied);
    }

    /// A failed cleanup after a failed atomic rename leaves an agent-visible
    /// temporary link behind, so it is a partial mutation too.
    #[test]
    fn a_residual_temporary_link_is_a_partial_repair() {
        let mut plan = empty_repair_plan(
            AgentKind::Codex,
            "portable",
            PathBuf::from("/home/example/.agents/skills/portable"),
        );
        plan.disposition = RepairDisposition::ReplaceLink { dangling: false };
        let applied = RepairApplyReport {
            step: Some(RepairAppliedStep {
                agent: AgentKind::Codex,
                link_path: plan.link_path.clone(),
                outcome: RepairStepOutcome::ResidualTemporary {
                    path: PathBuf::from("/home/example/.agents/skills/.skilled-repair-123-456"),
                    error: "permission denied".to_owned(),
                },
            }),
        };
        let outcome = RepairOutcome::new(plan, applied, VerifyReport::default());

        assert_eq!(outcome.status(), RepairStatus::PartiallyApplied);
    }
}
