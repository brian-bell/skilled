//! Install, repair, uninstall, and source-forget planning with guarded execution.
//!
//! Spec 17.2 reserves this module for pure plan builders and guarded executors,
//! and the split is kept literally: each probe reads the machine, each planner
//! decides over that observation, and no executor writes until the immutable
//! preview it belongs to has been confirmed.
//!
//! Two rules shape the whole module. A plan blocks whole rather than in part —
//! if any target is blocked, nothing is written anywhere, because spec 15 asks
//! Skilled to stop before writing when it already knows a step would fail. And
//! install never replaces an occupied slot. Install creates links; uninstall
//! removes only an exact receipted link without following it; forgetting removes
//! only private metadata after proving every described link inactive. Repair is a
//! separate single-target pipeline and replaces only a symbolic link whose raw
//! target still matches a Skilled ownership receipt exactly.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::{
    AgentDetection, AgentKind, MetadataFailure,
    agents::{adapter, detection_at},
    inventory::{
        Finding, FindingSeverity, InstallationHealth, InstallationObject,
        InstalledSkillObservation, InventoryRow, InventorySnapshot, MAX_ROOT_CHILDREN, Provenance,
        RootStatus,
    },
    resolution::{
        CandidateSelection, OpenCodeResolution, RootSighting, SightedEntry, UnknownCause,
        UnknownRoot, VariantRef, narrow, resolve_opencode, variants_by_name,
    },
    source::{
        RegisteredSource, RevisionLookup, SkillValidation, contains_revision, look_up_revision,
    },
    store::{RegistryFingerprint, Store},
    validation::{
        InspectionBudget, PortableValidationError, valid_skill_name,
        validate_portable_skill_with_budget,
    },
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
pub enum UninstallTargetState {
    Missing,
    Directory,
    NotADirectory,
    Unreadable(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum EntryProbe {
    /// Nothing occupies the slot.
    Absent,
    /// A symbolic link, with the target as recorded and where it resolves to.
    Symlink {
        target: PathBuf,
        canonical: Option<PathBuf>,
        target_state: UninstallTargetState,
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
    target_entries: [RepairEntryProbe; 3],
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
    let targets = agents.each_ref().map(|detection| {
        let mut target = probe_target(detection, skill_name, home);
        if detection.selected() {
            target.root = probe_repair_root(&target.root, detection.root());
        }
        target
    });
    let target_entries = agents.each_ref().map(|detection| {
        if detection.selected() {
            probe_repair_entry(&targets[detection.kind().index()].link_path)
        } else {
            RepairEntryProbe::NotRead
        }
    });
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
        target_entries,
        sources,
    }
}

/// Establish that repair can inspect the whole root it may write inside.
///
/// A known child can remain searchable and replaceable when directory read
/// permission is absent. Treating metadata alone as a readable root would then
/// allow a write whose mandatory inventory rescan is already known to be
/// unable to verify it. Iterating the directory also mirrors the inventory's
/// child bound, so repair does not call a partial view complete.
fn probe_repair_root(root_probe: &RootProbe, root: &Path) -> RootProbe {
    if !matches!(root_probe, RootProbe::Present) {
        return root_probe.clone();
    }
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) => return RootProbe::Unreadable(error.to_string()),
    };
    for (index, entry) in entries.enumerate() {
        if index == MAX_ROOT_CHILDREN {
            return RootProbe::Unreadable(format!(
                "the skill root holds more than {MAX_ROOT_CHILDREN} entries"
            ));
        }
        if let Err(error) = entry {
            return RootProbe::Unreadable(error.to_string());
        }
    }
    RootProbe::Present
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
    let safe_name = valid_skill_name(skill_name);
    let link_path = if safe_name {
        agent.root().join(skill_name)
    } else {
        agent.root().to_path_buf()
    };
    if !safe_name || link_path.parent() != Some(agent.root()) {
        return TargetProbe {
            agent: agent.kind(),
            link_path,
            root: RootProbe::NotRead,
            entry: EntryProbe::Unreadable(
                "the skill name is not one safe path component".to_owned(),
            ),
            content: SlotContent::Unknown,
        };
    }
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
        let target = match fs::read_link(link_path) {
            Ok(target) => target,
            Err(error) => return EntryProbe::Unreadable(error.to_string()),
        };
        let target_state = match fs::metadata(link_path) {
            Ok(metadata) if metadata.is_dir() => UninstallTargetState::Directory,
            Ok(_) => UninstallTargetState::NotADirectory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => UninstallTargetState::Missing,
            Err(error) => UninstallTargetState::Unreadable(error.to_string()),
        };
        return EntryProbe::Symlink {
            target,
            canonical: link_path.canonicalize().ok(),
            target_state,
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
    /// The registry this plan's selection was decided over, so the mutation
    /// guard can refuse a write the registry has since stopped agreeing with.
    registry: RegistryFingerprint,
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
    /// The registry this plan's selection was decided over. Install's field,
    /// for the same guard.
    registry: RegistryFingerprint,
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
    let mut plan = empty_repair_plan(
        agent,
        RegistryFingerprint::of_registry(sources),
        skill_name,
        target.link_path.clone(),
    );

    if let Some(finding) = repair_root_finding(&target.root) {
        plan.disposition = RepairDisposition::Blocked { finding };
        return plan;
    }

    let (observed_target, current_resolution) = match &probe.target_entries[agent.index()] {
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

fn empty_repair_plan(
    agent: AgentKind,
    registry: RegistryFingerprint,
    skill_name: &str,
    link_path: PathBuf,
) -> RepairPlan {
    RepairPlan {
        agent,
        registry,
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
        match &probe.target_entries[agent.index()] {
            RepairEntryProbe::Symlink {
                resolution: Err((io::ErrorKind::NotFound, _)),
                ..
            } => return RootSighting::NothingToLoad,
            RepairEntryProbe::Symlink {
                resolution: Err(_), ..
            }
            | RepairEntryProbe::Unreadable(_)
            | RepairEntryProbe::NotRead => return RootSighting::Unknown,
            _ => {}
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
    row_index: usize,
    agent: AgentKind,
    finding: Finding,
}

impl OverlayFinding {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn row_index(&self) -> usize {
        self.row_index
    }
    pub(crate) fn agent(&self) -> AgentKind {
        self.agent
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
        for (row_index, row) in snapshot.rows().iter().enumerate() {
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
                        row_index,
                        agent: observation.agent(),
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

/// Why one configured agent is not part of an uninstall request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UninstallExcludedReason {
    NotConfigured,
    NotRequested,
    NothingThere,
    NotSkilleds,
}

/// What uninstall will do about one documented agent slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UninstallDisposition {
    RemoveLink {
        link_target: PathBuf,
        target_state: UninstallTargetState,
        receipts: Vec<Receipt>,
    },
    Excluded {
        reason: UninstallExcludedReason,
    },
    Blocked {
        finding: Finding,
    },
}

/// One agent's uninstall target, always synthesized beneath its documented root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UninstallTarget {
    agent: AgentKind,
    link_path: PathBuf,
    disposition: UninstallDisposition,
}

impl UninstallTarget {
    pub fn agent(&self) -> AgentKind {
        self.agent
    }
    pub fn link_path(&self) -> &Path {
        &self.link_path
    }
    pub fn disposition(&self) -> &UninstallDisposition {
        &self.disposition
    }
    pub fn is_work(&self) -> bool {
        matches!(self.disposition, UninstallDisposition::RemoveLink { .. })
    }
}

/// The reduced fact an uninstall can truthfully predict about OpenCode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UninstallOutlook {
    Loads { winner: PathBuf },
    Disagreement,
    Nothing,
    Unknown,
}

impl UninstallOutlook {
    fn of(resolution: &OpenCodeResolution) -> Self {
        match resolution {
            OpenCodeResolution::Selected { winner, .. }
            | OpenCodeResolution::ForeignExposure { winner, .. }
            | OpenCodeResolution::IncompatibleExposure { winner, .. } => Self::Loads {
                winner: winner.path().to_path_buf(),
            },
            OpenCodeResolution::Conflict { .. } => Self::Disagreement,
            OpenCodeResolution::NothingVisible => Self::Nothing,
            OpenCodeResolution::Incomplete { .. } => Self::Unknown,
        }
    }
}

/// The read-only observation from which an uninstall plan is built.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UninstallProbe {
    targets: [TargetProbe; 3],
}

impl UninstallProbe {
    pub fn target(&self, agent: AgentKind) -> &TargetProbe {
        &self.targets[agent.index()]
    }
}

/// Read each selected native root once without following the final component.
pub fn probe_uninstall(
    agents: &[AgentDetection; 3],
    skill_name: &str,
    home: &Path,
) -> UninstallProbe {
    UninstallProbe {
        targets: agents
            .each_ref()
            .map(|agent| probe_target(agent, skill_name, home)),
    }
}

/// One immutable statement of every link uninstall would remove.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UninstallPlan {
    skill_name: String,
    targets: Vec<UninstallTarget>,
    warnings: Vec<String>,
    opencode_outlook: Option<UninstallOutlook>,
}

impl UninstallPlan {
    pub fn skill_name(&self) -> &str {
        &self.skill_name
    }
    pub fn targets(&self) -> &[UninstallTarget] {
        &self.targets
    }
    pub fn target(&self, agent: AgentKind) -> Option<&UninstallTarget> {
        self.targets.iter().find(|target| target.agent == agent)
    }
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
    pub fn opencode_outlook(&self) -> Option<&UninstallOutlook> {
        self.opencode_outlook.as_ref()
    }
    pub fn blocking_findings(&self) -> impl Iterator<Item = (AgentKind, &Finding)> {
        self.targets
            .iter()
            .filter_map(|target| match &target.disposition {
                UninstallDisposition::Blocked { finding } => Some((target.agent, finding)),
                _ => None,
            })
    }
    pub fn is_blocked(&self) -> bool {
        self.blocking_findings().next().is_some()
    }
    pub fn is_executable(&self) -> bool {
        !self.is_blocked() && self.targets.iter().any(UninstallTarget::is_work)
    }
}

/// Decide which exact receipted links can be removed, without reading the machine.
///
/// Ownership is proved against a receipt's recorded target rather than the
/// currently registered variant. A source may have moved or disappeared while
/// the receipt remains the evidence of the exact link Skilled created.
pub fn plan_uninstall(
    agents: &[AgentDetection; 3],
    receipts: &[Receipt],
    skill_name: &str,
    requested: [bool; 3],
    probe: &UninstallProbe,
) -> UninstallPlan {
    let mut targets = Vec::with_capacity(3);
    for agent in AgentKind::ALL {
        let detected = detection_at(agents, agent);
        let slot = probe.target(agent);
        let matching_path: Vec<&Receipt> = receipts
            .iter()
            .filter(|receipt| receipt.agent == agent && receipt.link_path == slot.link_path)
            .collect();
        let disposition = if !detected.selected() {
            UninstallDisposition::Excluded {
                reason: UninstallExcludedReason::NotConfigured,
            }
        } else if !requested[agent.index()] {
            UninstallDisposition::Excluded {
                reason: UninstallExcludedReason::NotRequested,
            }
        } else if matching_path.is_empty() {
            UninstallDisposition::Excluded {
                reason: match slot.entry {
                    EntryProbe::Absent => UninstallExcludedReason::NothingThere,
                    _ => UninstallExcludedReason::NotSkilleds,
                },
            }
        } else {
            uninstall_disposition(slot, &matching_path)
        };
        targets.push(UninstallTarget {
            agent,
            link_path: slot.link_path.clone(),
            disposition,
        });
    }

    let mut warnings = Vec::new();
    let blocked = targets
        .iter()
        .any(|target| matches!(target.disposition, UninstallDisposition::Blocked { .. }));
    if !blocked {
        for target in &targets {
            if let UninstallDisposition::RemoveLink {
                link_target,
                target_state,
                ..
            } = &target.disposition
            {
                let warning = match target_state {
                    UninstallTargetState::Directory => None,
                    UninstallTargetState::Missing => Some(format!(
                        "{} no longer resolves; this release will remove the managed link rather than repair it",
                        link_target.display()
                    )),
                    UninstallTargetState::NotADirectory => Some(format!(
                        "{} is no longer a directory; this release will remove the managed link rather than repair it",
                        link_target.display()
                    )),
                    UninstallTargetState::Unreadable(reason) => Some(format!(
                        "{} could not be read ({reason}); this release will remove the exact managed link, and content survival will be withheld",
                        link_target.display()
                    )),
                };
                if let Some(warning) = warning {
                    warnings.push(warning);
                }
            }
        }
    }
    let opencode_outlook = detection_at(agents, AgentKind::OpenCode)
        .selected()
        .then(|| {
            let before = UninstallOutlook::of(&resolve_opencode(uninstall_sightings(
                &targets, probe, false,
            )));
            let after = UninstallOutlook::of(&resolve_opencode(uninstall_sightings(
                &targets, probe, true,
            )));
            if !blocked && before != after && !matches!(after, UninstallOutlook::Unknown) {
                warnings.push(match &after {
                    UninstallOutlook::Loads { winner } => format!(
                        "after this uninstall, OpenCode would load {}",
                        winner.display()
                    ),
                    UninstallOutlook::Disagreement => {
                        "after this uninstall, OpenCode would see disagreeing directories"
                            .to_owned()
                    }
                    UninstallOutlook::Nothing => {
                        "after this uninstall, OpenCode would find nothing under this name"
                            .to_owned()
                    }
                    UninstallOutlook::Unknown => unreachable!(),
                });
            }
            after
        });
    UninstallPlan {
        skill_name: skill_name.to_owned(),
        targets,
        warnings,
        opencode_outlook,
    }
}

fn uninstall_disposition(slot: &TargetProbe, receipts: &[&Receipt]) -> UninstallDisposition {
    match &slot.root {
        RootProbe::Present => {}
        RootProbe::Redirected { via } => {
            return uninstall_blocked(
                "uninstall.redirected_root",
                format!("the skill root is redirected through {}", via.display()),
            );
        }
        RootProbe::Unreadable(reason) => {
            return uninstall_blocked(
                "uninstall.unreadable_root",
                format!("the skill root could not be read: {reason}"),
            );
        }
        RootProbe::Missing { .. } => {
            return UninstallDisposition::Excluded {
                reason: UninstallExcludedReason::NothingThere,
            };
        }
        RootProbe::NotRead => {
            return UninstallDisposition::Excluded {
                reason: UninstallExcludedReason::NotConfigured,
            };
        }
    }
    match &slot.entry {
        EntryProbe::Absent => UninstallDisposition::Excluded {
            reason: UninstallExcludedReason::NothingThere,
        },
        EntryProbe::Unreadable(reason) => uninstall_blocked(
            "uninstall.unreadable_entry",
            format!("the installation entry could not be read: {reason}"),
        ),
        EntryProbe::Directory { .. } | EntryProbe::NotADirectory => uninstall_blocked(
            "uninstall.not_a_symlink",
            "the receipted path is no longer a symbolic link".to_owned(),
        ),
        EntryProbe::NotRead => UninstallDisposition::Excluded {
            reason: UninstallExcludedReason::NotConfigured,
        },
        EntryProbe::Symlink {
            target,
            target_state,
            ..
        } => {
            let matching_receipts: Vec<Receipt> = receipts
                .iter()
                .filter(|receipt| receipt.link_target == *target)
                .map(|receipt| (*receipt).clone())
                .collect();
            if !matching_receipts.is_empty() {
                UninstallDisposition::RemoveLink {
                    link_target: target.clone(),
                    target_state: target_state.clone(),
                    receipts: matching_receipts,
                }
            } else {
                uninstall_blocked(
                    "uninstall.wrong_target",
                    format!("the link now points to {}", target.display()),
                )
            }
        }
    }
}

fn uninstall_blocked(code: &'static str, evidence: String) -> UninstallDisposition {
    UninstallDisposition::Blocked {
        finding: Finding::new(code, FindingSeverity::Critical, evidence),
    }
}

fn uninstall_sightings(
    targets: &[UninstallTarget],
    probe: &UninstallProbe,
    after: bool,
) -> [RootSighting; 3] {
    AgentKind::ALL.map(|agent| {
        let target = &targets[agent.index()];
        let slot = probe.target(agent);
        if matches!(
            slot.root,
            RootProbe::Unreadable(_) | RootProbe::NotRead | RootProbe::Redirected { .. }
        ) {
            return RootSighting::Unread;
        }
        if after && target.is_work() {
            return RootSighting::NothingToLoad;
        }
        match &slot.content {
            SlotContent::At(canonical) => RootSighting::Offers(SightedEntry::new(
                slot.link_path.clone(),
                canonical.clone(),
                None,
            )),
            SlotContent::Nowhere => RootSighting::NothingToLoad,
            SlotContent::Unknown => RootSighting::Unknown,
        }
    })
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
        registry: RegistryFingerprint::of_registry(sources),
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
/// now". A receipt may outlive a missing link until guarded uninstall or Forget
/// Source establishes that link gone; that makes it evidence for later repair.
/// A link removed and remade by hand at the same path still matches one. Skilled
/// records no inode or creation time, so it cannot tell those apart, and every
/// surface says only that a receipt exists for the path.
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
        EntryProbe::Symlink {
            target, canonical, ..
        } => {
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
                        "{what}: it points at {}. Install never replaces an existing entry. A \
                         proven Skilled-owned dangling or incorrect link can be handled by the \
                         separate repair operation; this install target is left exactly as it is",
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
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ReceiptFailure {
    #[error("{0}")]
    Metadata(MetadataFailure),
    #[error("{0}")]
    Other(String),
}

/// What happened to one target the plan called work.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StepOutcome {
    /// The link was created and its ownership receipt recorded.
    Created,
    /// The link was created but the receipt could not be written, so Skilled
    /// does not own something it put on disk. Stated rather than hidden: the
    /// link is real, and a later repair will not recognise it.
    CreatedUnrecorded(ReceiptFailure),
    /// A managed directory link was removed.
    Removed,
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

    fn removed_link(&self) -> bool {
        matches!(self.outcome, StepOutcome::Removed)
    }

    pub(crate) fn changed_filesystem(&self) -> bool {
        matches!(
            self.outcome,
            StepOutcome::Created
                | StepOutcome::CreatedUnrecorded(_)
                | StepOutcome::RootCreatedLinkFailed(_)
                | StepOutcome::Removed
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

    pub(crate) fn metadata_failure(&self) -> Option<&MetadataFailure> {
        self.steps.iter().find_map(|step| match &step.outcome {
            StepOutcome::CreatedUnrecorded(ReceiptFailure::Metadata(failure)) => Some(failure),
            _ => None,
        })
    }
}

/// One postcondition the fresh scan did not bear out.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifyFailure {
    agent: AgentKind,
    postcondition: Postcondition,
    observed: String,
}

impl VerifyFailure {
    pub fn agent(&self) -> AgentKind {
        self.agent
    }

    pub fn postcondition(&self) -> Postcondition {
        self.postcondition
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
    postcondition: Postcondition,
    reason: String,
    /// Whether this is a postcondition on a target Skilled wrote, as opposed
    /// to an ancillary OpenCode resolution affected through another root.
    required: bool,
    /// Whether the user's own agent selection is all that kept this check from
    /// running: every root involved went unread only because it was
    /// deselected. False whenever anything else contributed — an unreadable
    /// root, an entry that could not be followed, a scan that never ran.
    precluded_by_selection: bool,
}

impl VerifyWithheld {
    pub fn agent(&self) -> AgentKind {
        self.agent
    }

    pub fn postcondition(&self) -> Postcondition {
        self.postcondition
    }

    pub fn required(&self) -> bool {
        self.required
    }

    /// Whether the user's own agent selection is the only thing that kept
    /// this check from running.
    pub fn precluded_by_selection(&self) -> bool {
        self.precluded_by_selection
    }

    /// What stopped the check, in words.
    pub fn reason(&self) -> &str {
        &self.reason
    }

    fn blocks_success(&self) -> bool {
        self.required
    }
}

/// A separately identifiable postcondition, so no pass is inferred from silence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Postcondition {
    LinkAsPlanned,
    LinkGone,
    ContentSurvived,
    OpenCodeResolution,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifyPass {
    agent: AgentKind,
    postcondition: Postcondition,
}

impl VerifyPass {
    pub fn agent(self) -> AgentKind {
        self.agent
    }
    pub fn postcondition(self) -> Postcondition {
        self.postcondition
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
    held: Vec<VerifyPass>,
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

    /// Whether every postcondition the user's own agent selection lets Skilled
    /// check was both checked and held.
    ///
    /// [`Self::is_complete`] is the stricter question. The two differ exactly
    /// when the only unestablished checks are ones a deselected root
    /// precludes — the ordinary state for anyone running fewer than three
    /// agents, where the user asked Skilled to leave those roots alone. The
    /// installation inventory draws the same line: a deselected root does not
    /// make [`crate::InventorySnapshot::counts_are_complete`] false, while an
    /// unreadable one does.
    pub fn is_complete_for_selection(&self) -> bool {
        self.failures.is_empty()
            && self
                .withheld
                .iter()
                .all(VerifyWithheld::precluded_by_selection)
    }

    pub fn failures(&self) -> &[VerifyFailure] {
        &self.failures
    }

    pub fn withheld(&self) -> &[VerifyWithheld] {
        &self.withheld
    }

    pub fn held(&self) -> &[VerifyPass] {
        &self.held
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

/// The one modal operation dialog currently owning the keyboard.
///
/// Keeping every mutating flow behind one prompt preserves a single preview
/// confirmation and scrolling rule as uninstall and source forgetting are
/// added beside installation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OperationPrompt {
    Install(InstallPrompt),
    Uninstall(UninstallPrompt),
    Forget(ForgetPrompt),
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
pub(crate) fn apply_install(plan: &InstallPlan, store: &mut Store, home: &Path) -> ApplyReport {
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

// A test seam standing in for the second process the apply guards exist for.
//
// `apply_target` reads the link target before its metadata work and again
// after it, and only something running in the gap between those reads can show
// that the second one is load-bearing. Nothing outside a test build can
// register anything here — the hook does not exist there. It is thread-local
// and consumed once, so one test's stand-in can never reach another's install.
#[cfg(test)]
thread_local! {
    static CONCURRENT_TARGET_CHANGE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, unix))]
fn set_concurrent_target_change(change: impl FnOnce() + 'static) {
    CONCURRENT_TARGET_CHANGE.with(|slot| *slot.borrow_mut() = Some(Box::new(change)));
}

#[cfg(test)]
fn concurrent_target_change() {
    if let Some(change) = CONCURRENT_TARGET_CHANGE.with(|slot| slot.borrow_mut().take()) {
        change();
    }
}

#[cfg(not(test))]
#[inline]
fn concurrent_target_change() {}

fn apply_target(
    plan: &InstallPlan,
    target: &InstallTarget,
    store: &mut Store,
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
    if let Err(reason) = install_link_target_unchanged(plan) {
        return StepOutcome::Failed(reason);
    }
    concurrent_target_change();
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
    // Taken before the guard borrows the store: a receipt that cannot be
    // written is a failure of this database, and the report names it.
    let database_path = store.database_path().to_path_buf();
    let mutation = match store.begin_mutation() {
        Ok(mutation) => mutation,
        Err(error) => {
            return StepOutcome::Failed(format!(
                "the metadata mutation guard could not be acquired, so nothing was written: {error}"
            ));
        }
    };
    match mutation.source_is_registered(plan.variant.source_id()) {
        Ok(true) => {}
        Ok(false) => {
            return StepOutcome::Failed(
                "the source was forgotten after the plan was shown, so nothing was written"
                    .to_owned(),
            );
        }
        Err(error) => {
            return StepOutcome::Failed(format!(
                "the source registration could not be re-read, so nothing was written: {error}"
            ));
        }
    }
    // The identifier surviving is not the registration surviving. Re-registering
    // the same checkout keeps the row's id while replacing the catalog set this
    // variant was chosen from, and the receipt about to be written names that
    // catalog — so all of it is compared with what is registered now.
    match mutation.variant_registration_matches(&plan.variant, &plan.source_checkout) {
        Ok(true) => {}
        Ok(false) => {
            return StepOutcome::Failed(
                "the source's registration changed after the plan was shown, so nothing was \
                 written"
                    .to_owned(),
            );
        }
        Err(error) => {
            return StepOutcome::Failed(format!(
                "the source registration could not be re-read, so nothing was written: {error}"
            ));
        }
    }
    // The chosen row surviving is not the choice surviving. A source or catalog
    // another process registered after the preview can answer to this skill
    // name too, and the spec 6.4 selection this plan rests on would then be a
    // duplicate conflict or name a different variant. The registry is compared
    // whole under the guard, after the plan's own registration, so the more
    // specific refusal is the one a changed registration reports.
    match mutation.registry_fingerprint() {
        Ok(fingerprint) if fingerprint == plan.registry => {}
        Ok(_) => {
            return StepOutcome::Failed(
                "the registered sources changed after the plan was shown, so the variant this \
                 link would resolve to is no longer settled and nothing was written"
                    .to_owned(),
            );
        }
        Err(error) => {
            return StepOutcome::Failed(format!(
                "the registered sources could not be re-read, so nothing was written: {error}"
            ));
        }
    }
    if let Err(reason) = recheck_source_identity(plan) {
        return StepOutcome::Failed(format!("{reason}, so nothing was written"));
    }
    // Read once more, because everything above it waited on something outside
    // this process: the mutation guard waits on another Skilled's transaction,
    // and the identity recheck spawns Git. A concurrent `skilled update`
    // fast-forwarding this same checkout can delete the variant directory over
    // either wait, and a `symlink` that then succeeded would leave a dangling
    // link the preview never described — the exact outcome install refuses to
    // create. The check sits here rather than after the root creation below on
    // purpose: refusing costs the user nothing, while refusing one syscall
    // later would leave behind an empty documented root nobody asked for. What
    // is left between this read and the write is a `stat` and at most one
    // `create_dir`, which is the same pathname window `apply_install` records
    // as `skilled-cb2` rather than a wait on another process.
    if let Err(reason) = install_link_target_unchanged(plan) {
        return StepOutcome::Failed(reason);
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
    if let Err(error) = mutation.record_receipt(&receipt) {
        return StepOutcome::CreatedUnrecorded(ReceiptFailure::Metadata(MetadataFailure::new(
            database_path,
            error.to_string(),
        )));
    }
    match mutation.commit() {
        Ok(()) => StepOutcome::Created,
        Err(error) => StepOutcome::CreatedUnrecorded(ReceiptFailure::Metadata(
            MetadataFailure::new(database_path, error.to_string()),
        )),
    }
}

/// Re-establish that the directory every link would point at is still the one
/// the plan resolved, and still a portable skill.
///
/// Cheap enough to repeat, which is what lets [`apply_target`] ask it on both
/// sides of its metadata work; [`repair_source_unchanged`] repeats the same
/// reading for the same reason.
fn install_link_target_unchanged(plan: &InstallPlan) -> Result<(), String> {
    match plan.source_dir().canonicalize() {
        Ok(resolved)
            if resolved == plan.source_dir()
                && fs::metadata(&resolved).is_ok_and(|metadata| metadata.is_dir()) => {}
        _ => {
            return Err(format!(
                "{} is no longer the directory the plan resolved, so nothing was written",
                plan.source_dir().display()
            ));
        }
    }
    let mut budget = InspectionBudget::source_scan();
    validate_portable_skill_with_budget(plan.source_dir(), &mut budget)
        .map(|_| ())
        .map_err(|error| {
            format!(
                "{} no longer validates as a portable skill, so nothing was written: {error}",
                plan.source_dir().display()
            )
        })
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
    /// A failed replacement attempt left an object preserved at a sibling
    /// temporary path, which must be reported where it actually is. Only that
    /// residual object is known to remain: the destination is unchanged on
    /// most paths, but after a failed revert its state is unproven — it may
    /// hold neither the original link nor the replacement.
    ResidualTemporary {
        path: PathBuf,
        error: String,
    },
    /// The replacement is live and its receipt recorded, but the exchange left
    /// a displaced object at a sibling temporary path that could not be
    /// removed or swapped back. The residual write must be reported with its
    /// exact path.
    RepairedResidualTemporary {
        path: PathBuf,
        error: String,
    },
    /// The replacement was written, but the skill root was renamed while the
    /// repair ran: the live, unreceipted link is at `path` — a location the
    /// plan never stated — and must never be presented as removable residue.
    MovedRootUnreceipted {
        path: PathBuf,
        error: String,
    },
    Failed(String),
}

impl RepairStepOutcome {
    fn wrote_link(&self) -> bool {
        matches!(
            self,
            Self::Repaired | Self::RepairedUnrecorded(_) | Self::RepairedResidualTemporary { .. }
        )
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
/// Unix creates a sibling temporary link and atomically exchanges it with the
/// destination, so readers never observe the installation path absent. Unlike
/// the rename this replaced, the exchange destroys nothing: the displaced
/// object is verified byte-identical to the proven raw target before it is
/// removed, anything else is swapped back intact and refused, and a
/// filesystem that cannot exchange refuses the repair rather than falling
/// back to a destructive rename. `replace_directory_symlink` carries the full
/// syscall-level argument that closed the skilled-2k3.6.1 window.
///
/// Windows cannot atomically rename over an existing directory symlink, so
/// its replacement stays remove-then-create — but the removal no longer
/// trusts the pathname. The destination is pinned once with
/// `FILE_FLAG_OPEN_REPARSE_POINT`, proven through that handle to be the
/// exact symbolic link the recheck read, and deleted through the same handle
/// with a POSIX-semantics disposition, so an object that arrived after the
/// recheck is refused rather than deleted
/// (`remove_proven_directory_symlink` carries the argument; skilled-tdm was
/// the `remove_dir` window this closed). What remains is an install-class
/// fail-if-exists window at creation, and a creation failure after the
/// removal is a partial apply reported as such rather than as an inert
/// refusal.
///
/// The metadata mutation guard covers the replacement for the same reason it
/// covers install's creation. A repair makes a link active and records a
/// receipt naming a registered source, which is exactly the state Forget Source
/// proves absent before it deletes that source's metadata. Taking the guard
/// before the final rechecks and holding it through the receipt commit is what
/// keeps a repair from landing inside Forget's window and leaving an active
/// link into a source Skilled no longer knows.
pub(crate) fn apply_repair(plan: &RepairPlan, store: &mut Store, home: &Path) -> RepairApplyReport {
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

fn apply_repair_target(plan: &RepairPlan, store: &mut Store, home: &Path) -> RepairStepOutcome {
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
    if let Err(reason) = repair_source_unchanged(source_dir, source_checkout, source_revision) {
        return RepairStepOutcome::Failed(reason);
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
    // From here to the receipt commit, no other Skilled process may decide this
    // source's metadata is safe to delete: the replacement below makes the link
    // active, which is the fact Forget Source proves absent before it deletes.
    let mutation = match store.begin_mutation() {
        Ok(mutation) => mutation,
        Err(error) => {
            return RepairStepOutcome::Failed(format!(
                "the metadata mutation guard could not be acquired, so nothing was written: {error}"
            ));
        }
    };
    match mutation.source_is_registered(variant.source_id()) {
        Ok(true) => {}
        Ok(false) => {
            return RepairStepOutcome::Failed(
                "the source was forgotten after the plan was shown, so nothing was written"
                    .to_owned(),
            );
        }
        Err(error) => {
            return RepairStepOutcome::Failed(format!(
                "the source registration could not be re-read, so nothing was written: {error}"
            ));
        }
    }
    // Install's reasoning, for the receipt this repair records: a re-register of
    // the same checkout keeps the source's id while replacing the catalog the
    // replacement variant was chosen from.
    match mutation.variant_registration_matches(variant, source_checkout) {
        Ok(true) => {}
        Ok(false) => {
            return RepairStepOutcome::Failed(
                "the source's registration changed after the plan was shown, so nothing was \
                 written"
                    .to_owned(),
            );
        }
        Err(error) => {
            return RepairStepOutcome::Failed(format!(
                "the source registration could not be re-read, so nothing was written: {error}"
            ));
        }
    }
    // Install's reasoning again, for the selection rather than the row: a
    // source registered after the preview can offer a competing variant of this
    // name, and the replacement this repair would install is then one candidate
    // of two rather than the one the registry resolves to.
    match mutation.registry_fingerprint() {
        Ok(fingerprint) if fingerprint == plan.registry => {}
        Ok(_) => {
            return RepairStepOutcome::Failed(
                "the registered sources changed after the plan was shown, so the variant this \
                 link would resolve to is no longer settled and nothing was written"
                    .to_owned(),
            );
        }
        Err(error) => {
            return RepairStepOutcome::Failed(format!(
                "the registered sources could not be re-read, so nothing was written: {error}"
            ));
        }
    }
    // Source validation, receipt checks, and Git identity verification can all
    // take time, and acquiring the guard above can wait behind another writer
    // for as long as that writer holds it. The guard freezes the metadata, not
    // the filesystem, so both sides of the write — where the link points and
    // where it lands — are re-established here. An object arriving at the
    // destination after these rechecks is handled by the replacement itself,
    // which proves what it displaced before destroying anything.
    if let Err(reason) = repair_source_unchanged(source_dir, source_checkout, source_revision) {
        return RepairStepOutcome::Failed(reason);
    }
    // The revision surviving does not make the working tree valid: a dirty
    // checkout can lose the variant's `SKILL.md` without moving HEAD. The scan
    // is bounded, so it is repeated rather than trusted from before the wait.
    let mut guarded_budget = InspectionBudget::source_scan();
    if let Err(error) = validate_portable_skill_with_budget(source_dir, &mut guarded_budget) {
        return RepairStepOutcome::Failed(format!(
            "{} no longer validates as a portable skill, so nothing was written: {error}",
            source_dir.display()
        ));
    }
    if let Err(reason) = repair_destination_unchanged(plan, root, home) {
        return RepairStepOutcome::Failed(reason);
    }
    let replacement = match replace_directory_symlink(
        source_dir,
        &plan.link_path,
        plan.recorded_target(),
        home,
    ) {
        Ok(replacement) => replacement,
        Err(ReplaceLinkError::Unchanged(error)) => {
            return RepairStepOutcome::Failed(format!("the link could not be replaced: {error}"));
        }
        #[cfg(unix)]
        Err(ReplaceLinkError::ExchangeUnsupported(error)) => {
            return RepairStepOutcome::Failed(format!(
                "the skill root's filesystem does not support atomic exchange, so a provable \
                 replacement is impossible and nothing was written: {error}"
            ));
        }
        #[cfg(any(unix, windows))]
        Err(ReplaceLinkError::ConcurrentlyReplaced { observed }) => {
            return RepairStepOutcome::Failed(format!(
                "the destination changed after the final recheck — {observed}; the arriving \
                 object was left in place and nothing was written"
            ));
        }
        #[cfg(windows)]
        Err(ReplaceLinkError::RemovedOldLink(error)) => {
            return RepairStepOutcome::RemovedUnreplaced(error.to_string());
        }
        #[cfg(unix)]
        Err(ReplaceLinkError::ResidualTemporary { path, detail }) => {
            return RepairStepOutcome::ResidualTemporary {
                path,
                error: detail,
            };
        }
    };
    // A residual tail still replaced the link: the receipt describing the live
    // replacement is recorded before the leftover is reported. Every reported
    // path is resolved from the pin at the moment it is reported, so a root
    // renamed at any point still has its entries named where they now are.
    #[cfg(unix)]
    let residual = |detail: &String| (replacement.pin.residual_path(), detail.clone());
    #[cfg(not(unix))]
    let residual = |detail: &String| (plan.link_path.clone(), detail.clone());
    let describe = |error: &dyn std::fmt::Display| match &replacement.residue {
        None => error.to_string(),
        Some(detail) => {
            let (path, detail) = residual(detail);
            format!("{error}; additionally, {detail} at {}", path.display())
        }
    };
    // A failed receipt write or commit must still classify a moved root:
    // reporting `RepairedUnrecorded` alone would imply the replacement is at
    // the planned path when it may be live in the renamed directory.
    let unrecorded = |error: &dyn std::fmt::Display| {
        #[cfg(unix)]
        if !replacement.pin.still_at_planned_path() {
            return RepairStepOutcome::MovedRootUnreceipted {
                path: replacement.pin.written_path(),
                error: format!(
                    "the skill root was renamed while the repair ran, so the planned path no \
                     longer names the directory the guards validated; additionally, the receipt \
                     could not be recorded: {}",
                    describe(error)
                ),
            };
        }
        RepairStepOutcome::RepairedUnrecorded(describe(error))
    };
    if let Err(error) = mutation.record_receipt(&receipt) {
        return unrecorded(&error);
    }
    // The identity guard the replacement held has to still hold when the
    // receipt becomes durable: a root renamed since would leave the receipt
    // naming a path the live replacement no longer has. Dropping the mutation
    // without committing rolls the staged receipt back. A rename after this
    // recheck is indistinguishable from one moments after a completed repair —
    // a receipt is historical evidence, revalidated against the live
    // filesystem before any later action relies on it — which is the boundary
    // of what any pre-commit check can promise.
    #[cfg(unix)]
    if !replacement.pin.still_at_planned_path() {
        let residue = match &replacement.residue {
            None => String::new(),
            Some(detail) => {
                let (path, detail) = residual(detail);
                format!("; additionally, {detail} at {}", path.display())
            }
        };
        return RepairStepOutcome::MovedRootUnreceipted {
            path: replacement.pin.written_path(),
            error: format!(
                "the skill root was renamed while the repair ran, so the planned path no longer \
                 names the directory the guards validated{residue}"
            ),
        };
    }
    match mutation.commit() {
        Ok(()) => match &replacement.residue {
            None => RepairStepOutcome::Repaired,
            Some(detail) => {
                let (path, error) = residual(detail);
                RepairStepOutcome::RepairedResidualTemporary { path, error }
            }
        },
        Err(error) => unrecorded(&error),
    }
}

/// Re-establish that the replacement still points where the plan resolved.
///
/// The directory the new link would name has to still be exactly that directory
/// — canonicalizing to itself, and a directory rather than something that
/// replaced it — and the checkout it belongs to has to still be the one the plan
/// named. Cheap enough to repeat under the mutation guard, which is where it
/// matters: everything checked before that guard was checked before an
/// unbounded wait.
fn repair_source_unchanged(
    source_dir: &Path,
    checkout: &Path,
    revision: &str,
) -> Result<(), String> {
    match source_dir.canonicalize() {
        Ok(resolved)
            if resolved == source_dir
                && fs::metadata(&resolved).is_ok_and(|metadata| metadata.is_dir()) => {}
        _ => {
            return Err(format!(
                "{} is no longer the directory the plan resolved, so nothing was written",
                source_dir.display()
            ));
        }
    }
    verified_checkout(checkout, revision)
        .map(|_| ())
        .map_err(|reason| format!("{reason}, so nothing was written"))
}

fn repair_destination_unchanged(plan: &RepairPlan, root: &Path, home: &Path) -> Result<(), String> {
    let root_probe = probe_root(root, home);
    if probe_repair_root(&root_probe, root) != RootProbe::Present {
        return Err(
            "the agent's skill root changed after the plan was shown, so nothing was written"
                .to_owned(),
        );
    }
    match (probe_repair_entry(&plan.link_path), &plan.disposition) {
        (
            RepairEntryProbe::Symlink {
                target,
                resolution: Err((io::ErrorKind::NotFound, _)),
            },
            RepairDisposition::ReplaceLink { dangling: true },
        ) if target == plan.recorded_target() => Ok(()),
        (
            RepairEntryProbe::Symlink {
                target,
                resolution: Ok(_),
            },
            RepairDisposition::ReplaceLink { dangling: false },
        ) if target == plan.recorded_target() => Ok(()),
        _ => Err(
            "the entry or its resolution changed after the plan was shown, so nothing was written"
                .to_owned(),
        ),
    }
}

/// Atomically exchange two entries of the pinned directory, destroying
/// neither.
///
/// This is the syscall-level guard skilled-2k3.6.1 asked for: unlike
/// `rename(2)`, which unlinks whatever occupies its destination, an exchange
/// only swaps two directory entries. macOS spells it `renameatx_np(2)` with
/// `RENAME_SWAP`; Linux spells it `renameat2(2)` with `RENAME_EXCHANGE`. The
/// flag constants come from `libc` for the reason `src/git.rs` records: they
/// are per-kernel ABI values that must not be transcribed by hand. A Unix
/// platform with neither call gets `ErrorKind::Unsupported`, which repair
/// reports as a refusal rather than falling back to a destructive rename.
#[cfg(target_os = "macos")]
fn exchange_in(dir: &fs::File, a: &std::ffi::CStr, b: &std::ffi::CStr) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: the descriptor and both name pointers are live, and the flag is
    // the documented RENAME_SWAP value for this platform.
    let result = unsafe {
        libc::renameatx_np(
            dir.as_raw_fd(),
            a.as_ptr(),
            dir.as_raw_fd(),
            b.as_ptr(),
            libc::RENAME_SWAP,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn exchange_in(dir: &fs::File, a: &std::ffi::CStr, b: &std::ffi::CStr) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: the descriptor and both name pointers are live, and the flag is
    // the documented RENAME_EXCHANGE value for this platform.
    let result = unsafe {
        libc::renameat2(
            dir.as_raw_fd(),
            a.as_ptr(),
            dir.as_raw_fd(),
            b.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn exchange_in(_dir: &fs::File, _a: &std::ffi::CStr, _b: &std::ffi::CStr) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this platform offers no atomic path exchange",
    ))
}

/// Open the trust anchor the pinned walk starts from.
///
/// The base — the validated home directory — is deliberately opened without
/// `O_NOFOLLOW`: the existing path contract permits home itself to be
/// reached through a link and refuses only redirected components below it,
/// and this open follows that same rule. Every component beneath the base is
/// then opened `O_NOFOLLOW` relative to the previous descriptor.
///
/// `git.rs` records the semantics the resulting handle provides: an open
/// directory handle keeps naming the directory the guards validated,
/// whatever happens to the pathname, so a skill root renamed mid-operation
/// can neither redirect a write nor strand an entry at a path the report
/// does not name. Every create, exchange, inspection, and unlink of the
/// replacement runs relative to the walked descriptor.
#[cfg(unix)]
fn open_trusted_base(base: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(base)
}

/// Open the next path component relative to an already-pinned directory,
/// refusing a symbolic link at that component.
#[cfg(unix)]
fn open_dir_component(dir: &fs::File, name: &std::ffi::OsStr) -> io::Result<fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = entry_name_arg(name)?;
    // SAFETY: the descriptor and name pointer are live; the returned fd is
    // owned immediately by the File.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}

/// Pin the skill root's parent by walking to it from the validated base one
/// component at a time, each step descriptor-relative and `O_NOFOLLOW`.
///
/// A single `open` with `O_NOFOLLOW` guards only its final component, so an
/// ancestor between the base and the root swapped for a symbolic link after
/// the guards would silently redirect the pin. The walk closes that: every
/// component is opened relative to the previous descriptor and refuses to be
/// a link, which is the repository's root-is-not-a-link invariant enforced at
/// the syscall level — and the `openat` discipline `skilled-cb2` asks for.
#[cfg(unix)]
fn open_pinned_parent_via(base: &Path, parent: &Path) -> io::Result<fs::File> {
    let relative = parent.strip_prefix(base).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the skill root does not lie under the validated base",
        )
    })?;
    let mut dir = open_trusted_base(base)?;
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the path contains a non-plain component",
            ));
        };
        dir = open_dir_component(&dir, name)?;
    }
    Ok(dir)
}

/// A single directory-entry name as a syscall argument.
#[cfg(unix)]
fn entry_name_arg(name: &std::ffi::OsStr) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "the name contains a NUL byte"))
}

/// Create a symbolic link to `target` named `name` in the pinned directory.
#[cfg(unix)]
fn symlink_in(dir: &fs::File, target: &Path, name: &std::ffi::CStr) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    let target = std::ffi::CString::new(target.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the target contains a NUL byte",
        )
    })?;
    // SAFETY: the descriptor and both pointers are live.
    if unsafe { libc::symlinkat(target.as_ptr(), dir.as_raw_fd(), name.as_ptr()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Read the raw target of the link named `name` in the pinned directory.
#[cfg(unix)]
fn read_link_in(dir: &fs::File, name: &std::ffi::CStr) -> io::Result<PathBuf> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStringExt;
    let mut capacity = 256usize;
    loop {
        let mut buffer = vec![0u8; capacity];
        // SAFETY: the descriptor, the name, and the buffer with its exact
        // length are all live.
        let written = unsafe {
            libc::readlinkat(
                dir.as_raw_fd(),
                name.as_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        };
        let Ok(written) = usize::try_from(written) else {
            return Err(io::Error::last_os_error());
        };
        if written < buffer.len() {
            buffer.truncate(written);
            return Ok(PathBuf::from(std::ffi::OsString::from_vec(buffer)));
        }
        // A result filling the buffer may be truncated; retry larger, bounded
        // well past any target Skilled itself would write.
        capacity *= 2;
        if capacity > 1 << 16 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the link target does not fit any supported path",
            ));
        }
    }
}

/// Unlink the entry named `name` in the pinned directory.
#[cfg(unix)]
fn unlink_in(dir: &fs::File, name: &std::ffi::CStr) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: the descriptor and the name pointer are live.
    if unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// The pinned directory's current pathname, so a residue report can name
/// where an object actually is even after the directory was renamed. `None`
/// when the platform cannot answer; the caller then falls back to the
/// pathname the plan stated.
#[cfg(target_os = "macos")]
fn pinned_directory_path(dir: &fs::File) -> Option<PathBuf> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStringExt;
    let mut buffer = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH requires a buffer of at least PATH_MAX bytes, which
    // this is; the descriptor is live.
    if unsafe { libc::fcntl(dir.as_raw_fd(), libc::F_GETPATH, buffer.as_mut_ptr()) } == -1 {
        return None;
    }
    let length = buffer.iter().position(|byte| *byte == 0)?;
    buffer.truncate(length);
    Some(PathBuf::from(std::ffi::OsString::from_vec(buffer)))
}

#[cfg(target_os = "linux")]
fn pinned_directory_path(dir: &fs::File) -> Option<PathBuf> {
    use std::os::fd::AsRawFd;
    fs::read_link(format!("/proc/self/fd/{}", dir.as_raw_fd())).ok()
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn pinned_directory_path(_dir: &fs::File) -> Option<PathBuf> {
    None
}

/// Whether `path` still names the pinned directory itself, established by
/// re-walking every component from the trust anchor with the same
/// `O_NOFOLLOW` discipline the pin was acquired with, then comparing the
/// resulting descriptor's device and inode against the pin. A final-component
/// check alone would follow a symlink swapped in at any ancestor, so the
/// whole chain is re-proven: nothing below the base may be a link, and the
/// walk must land on the very directory the writes went through. Success,
/// and the receipt it records against the planned pathname, may be claimed
/// only while this holds.
#[cfg(unix)]
fn pinned_directory_still_via(dir: &fs::File, base: &Path, path: &Path) -> bool {
    let Ok(reopened) = open_pinned_parent_via(base, path) else {
        return false;
    };
    same_directory(dir, &reopened)
}

#[cfg(unix)]
fn same_directory(a: &fs::File, b: &fs::File) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (a.metadata(), b.metadata()) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// Whether an exchange failure means the filesystem or kernel cannot exchange
/// at all, as opposed to this particular attempt failing. `ENOTSUP` is the
/// documented macOS answer for a volume without `VOL_CAP_INT_RENAME_SWAP`;
/// Linux answers `EINVAL` from a filesystem without `RENAME_EXCHANGE` support
/// and `ENOSYS` from a kernel predating `renameat2(2)`.
#[cfg(unix)]
fn exchange_unsupported(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::Unsupported
        || matches!(
            error.raw_os_error(),
            Some(libc::ENOTSUP | libc::EINVAL | libc::ENOSYS)
        )
}

/// A completed replacement, still pinned.
#[derive(Debug)]
struct LinkReplacement {
    /// The pin the writes went through, kept so the caller can re-verify the
    /// planned pathname immediately before the receipt becomes durable and
    /// resolve every reported path from the descriptor at reporting time.
    #[cfg(unix)]
    pin: ReplacementPin,
    /// What remains at the sibling temporary path, if anything; its current
    /// path is resolved from the pin when it is reported.
    residue: Option<String>,
}

/// The pinned directory a replacement was performed in, with the entry names
/// it used.
#[cfg(unix)]
#[derive(Debug)]
struct ReplacementPin {
    dir: fs::File,
    base: PathBuf,
    planned_parent: PathBuf,
    destination_file_name: std::ffi::OsString,
    temporary_file_name: String,
}

#[cfg(unix)]
impl ReplacementPin {
    /// Whether the planned pathname still names the pinned directory itself,
    /// re-walked component by component from the trust anchor.
    fn still_at_planned_path(&self) -> bool {
        pinned_directory_still_via(&self.dir, &self.base, &self.planned_parent)
    }

    /// The directory's current pathname. The planned pathname is used only
    /// after re-verifying it still names the pinned directory; when neither
    /// resolution works, `None` — a stale path must never pose as an object's
    /// actual location.
    fn current_directory(&self) -> Option<PathBuf> {
        pinned_directory_path(&self.dir).or_else(|| {
            self.still_at_planned_path()
                .then(|| self.planned_parent.clone())
        })
    }

    fn locate(&self, name: &std::ffi::OsStr) -> PathBuf {
        match self.current_directory() {
            Some(directory) => directory.join(name),
            None => Path::new("<the moved directory could not be resolved>").join(name),
        }
    }

    /// Where the written replacement lives right now.
    fn written_path(&self) -> PathBuf {
        self.locate(&self.destination_file_name)
    }

    /// Where the temporary entry lives right now.
    fn residual_path(&self) -> PathBuf {
        self.locate(std::ffi::OsStr::new(&self.temporary_file_name))
    }
}

/// Replace the proven link with a freshly created one, destroying nothing
/// unproven.
///
/// A plain `rename(2)` unlinks whatever occupies its destination, so an object
/// arriving between the caller's last recheck and the syscall would be
/// destroyed. The exchange closes that window: the temporary link is swapped
/// with the destination atomically, the displaced object — moved intact to a
/// sibling temporary path — is read back, and only a symbolic link whose raw
/// target is byte-identical to `proven_target` is then removed. Anything else
/// was a concurrent arrival: the exchange is reverted, preserving the object,
/// and the attempt is refused. Readers never observe the destination absent
/// on either path.
///
/// The exchange binds the destination — the one entry a concurrent actor
/// legitimately touches — and a revert exchange touches it again, so what a
/// revert displaces to the temporary path may be a later arrival rather than
/// Skilled's own replacement. Every cleanup of the temporary path therefore
/// goes through [`remove_proven_temporary`], which removes only a link
/// re-proven byte-identical to the object that cleanup expects — the recorded
/// old target, or Skilled's own replacement — and preserves anything else as
/// reported residue. Every operation here — create, exchange, inspection,
/// cleanup — runs relative to a parent directory pinned open first, so a
/// skill root renamed mid-operation can neither redirect a write nor strand
/// an entry unreported ([`open_pinned_parent`] records the semantics this
/// borrows from `git.rs`), and success is claimed only while the planned
/// pathname still names the pinned directory — a mid-operation rename becomes
/// a partial stating where the replacement actually lives, recording no
/// receipt against the stale path. The pinned directory proves itself before
/// anything is created: the descriptor was opened from a pathname after the
/// guards ran against that pathname, and the gap between the two closes by
/// requiring the pinned directory's own destination entry to still hold a
/// link byte-identical to the proven raw target. Linux and macOS offer no
/// unlink bound to a verified inode (FreeBSD's `funlinkat(2)` has no
/// equivalent here); the check-then-unlink remainder and the private-name
/// argument that bounds it are documented on that helper. The residual tails
/// — a displaced object that cannot be removed or swapped back — leave extra
/// entries behind and still destroy nothing; their reported path is resolved
/// from the pinned descriptor at reporting time, so it names the directory
/// wherever it now lives.
#[cfg(unix)]
fn replace_directory_symlink(
    target: &Path,
    link_path: &Path,
    proven_target: &Path,
    base: &Path,
) -> Result<LinkReplacement, ReplaceLinkError> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let parent = link_path.parent().ok_or_else(|| {
        ReplaceLinkError::Unchanged(io::Error::new(
            io::ErrorKind::InvalidInput,
            "target has no parent",
        ))
    })?;
    let Some(destination_file_name) = link_path.file_name() else {
        return Err(ReplaceLinkError::Unchanged(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the destination has no file name",
        )));
    };
    let destination_name =
        entry_name_arg(destination_file_name).map_err(ReplaceLinkError::Unchanged)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary_file_name = format!(".skilled-repair-{}-{nonce}", std::process::id());
    let temporary_name = entry_name_arg(std::ffi::OsStr::new(&temporary_file_name))
        .map_err(ReplaceLinkError::Unchanged)?;
    let dir = open_pinned_parent_via(base, parent).map_err(ReplaceLinkError::Unchanged)?;
    // A residue report must name where the object actually is, which the
    // starting pathname cannot promise once the directory is pinned: resolve
    // the pinned directory's current path at reporting time instead.
    let residual_path = || {
        // The planned pathname is trusted only after re-verifying it still
        // names the pinned directory; an unresolvable moved directory is
        // stated as such rather than posed as a stale path.
        let directory = pinned_directory_path(&dir).or_else(|| {
            pinned_directory_still_via(&dir, base, parent).then(|| parent.to_path_buf())
        });
        match directory {
            Some(directory) => directory.join(&temporary_file_name),
            None => {
                Path::new("<the moved directory could not be resolved>").join(&temporary_file_name)
            }
        }
    };
    // The guards ran against a pathname, and this descriptor was opened from
    // that pathname afterwards; the gap between the two is closed by making
    // the pinned directory prove itself. Only a directory whose entry still
    // holds a link byte-identical to the proven raw target may be written —
    // the same ownership standard every other repair decision applies.
    match read_link_in(&dir, &destination_name) {
        Ok(raw) if raw == proven_target => {}
        Ok(raw) => {
            return Err(ReplaceLinkError::ConcurrentlyReplaced {
                observed: format!("a symbolic link to {} arrived there", raw.display()),
            });
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ReplaceLinkError::Unchanged(error));
        }
        Err(error) if error.raw_os_error() == Some(libc::EINVAL) => {
            return Err(ReplaceLinkError::ConcurrentlyReplaced {
                observed: "an object that is not a symbolic link arrived there".to_owned(),
            });
        }
        Err(error) => return Err(ReplaceLinkError::Unchanged(error)),
    }
    symlink_in(&dir, target, &temporary_name).map_err(ReplaceLinkError::Unchanged)?;
    if let Err(error) = exchange_in(&dir, &temporary_name, &destination_name) {
        // The destination is untouched; the only write so far is the
        // temporary link, and even that is re-proven before removal.
        return Err(
            match remove_proven_temporary(&dir, &temporary_name, target) {
                TemporaryCleanup::Removed if exchange_unsupported(&error) => {
                    ReplaceLinkError::ExchangeUnsupported(error)
                }
                TemporaryCleanup::Removed => ReplaceLinkError::Unchanged(error),
                TemporaryCleanup::NotProven { observed } => ReplaceLinkError::ResidualTemporary {
                    path: residual_path(),
                    detail: format!(
                        "the atomic exchange failed ({error}), and by then {observed} the temporary \
                     path, so it was preserved"
                    ),
                },
                TemporaryCleanup::RemoveFailed(cleanup_error) => {
                    ReplaceLinkError::ResidualTemporary {
                        path: residual_path(),
                        detail: format!(
                            "the atomic exchange failed ({error}), then removing the temporary link \
                     failed: {cleanup_error}"
                        ),
                    }
                }
            },
        );
    }
    // The exchange moved whatever occupied the destination to the temporary
    // path: this read describes exactly the object the replacement displaced.
    let outcome = match read_link_in(&dir, &temporary_name) {
        Ok(raw) if raw == proven_target => {
            match remove_proven_temporary(&dir, &temporary_name, proven_target) {
                TemporaryCleanup::Removed => Ok(None),
                TemporaryCleanup::NotProven { observed } => Ok(Some(format!(
                    "after the displaced proven link was verified, {observed} the temporary \
                     path, so it was preserved"
                ))),
                TemporaryCleanup::RemoveFailed(cleanup_error) => {
                    match exchange_in(&dir, &temporary_name, &destination_name) {
                        // The revert touched the public destination again, so what
                        // it displaced to the temporary path must be re-proven as
                        // Skilled's own replacement before it may be removed.
                        Ok(()) => match remove_proven_temporary(&dir, &temporary_name, target) {
                            TemporaryCleanup::Removed => {
                                Err(ReplaceLinkError::Unchanged(io::Error::new(
                                    cleanup_error.kind(),
                                    format!(
                                        "the displaced link could not be removed ({cleanup_error}), \
                                     so the exchange was reverted"
                                    ),
                                )))
                            }
                            TemporaryCleanup::NotProven { observed } => {
                                Err(ReplaceLinkError::ResidualTemporary {
                                    path: residual_path(),
                                    detail: format!(
                                        "the displaced proven link could not be removed \
                                     ({cleanup_error}); the exchange was reverted, and it \
                                     displaced an object that arrived at the destination \
                                     meanwhile — {observed} the temporary path and was preserved"
                                    ),
                                })
                            }
                            TemporaryCleanup::RemoveFailed(second_error) => {
                                Err(ReplaceLinkError::ResidualTemporary {
                                    path: residual_path(),
                                    detail: format!(
                                        "the displaced proven link could not be removed \
                                     ({cleanup_error}); the exchange was reverted, but the \
                                     temporary replacement link could not be removed either: \
                                     {second_error}"
                                    ),
                                })
                            }
                        },
                        // A failed revert proves nothing about the
                        // destination: success — and the receipt it records —
                        // is claimed only after re-reading, through the pinned
                        // descriptor, that the replacement is still there.
                        Err(revert_error) => match read_link_in(&dir, &destination_name) {
                            Ok(raw) if raw == target => Ok(Some(format!(
                                "the displaced proven old link could not be removed \
                                 ({cleanup_error}) and the exchange could not be reverted \
                                 ({revert_error})"
                            ))),
                            _ => Err(ReplaceLinkError::ResidualTemporary {
                                path: residual_path(),
                                detail: format!(
                                    "the displaced proven link could not be removed \
                                     ({cleanup_error}), the exchange could not be reverted \
                                     ({revert_error}), and the destination no longer holds the \
                                     replacement; the displaced link was preserved at this path"
                                ),
                            }),
                        },
                    }
                }
            }
        }
        displaced => {
            let observed = match displaced {
                Ok(raw) => format!("a symbolic link to {} arrived there", raw.display()),
                Err(_) => "an object that is not a symbolic link arrived there".to_owned(),
            };
            match exchange_in(&dir, &temporary_name, &destination_name) {
                // As above: the revert exchanged with the public destination,
                // so only Skilled's own replacement may be removed from the
                // temporary path afterwards; anything else arrived at the
                // destination during the attempt and is preserved.
                Ok(()) => match remove_proven_temporary(&dir, &temporary_name, target) {
                    TemporaryCleanup::Removed => {
                        Err(ReplaceLinkError::ConcurrentlyReplaced { observed })
                    }
                    TemporaryCleanup::NotProven { observed: occupant } => {
                        Err(ReplaceLinkError::ResidualTemporary {
                            path: residual_path(),
                            detail: format!(
                                "{observed}; it was restored, and the revert displaced a further \
                             arrival — {occupant} the temporary path and was preserved"
                            ),
                        })
                    }
                    TemporaryCleanup::RemoveFailed(cleanup_error) => {
                        Err(ReplaceLinkError::ResidualTemporary {
                            path: residual_path(),
                            detail: format!(
                                "{observed}; it was restored, but the temporary replacement link \
                                 could not be removed: {cleanup_error}"
                            ),
                        })
                    }
                },
                // As above: a failed revert proves nothing about the
                // destination, so the receipt-recording success is claimed
                // only after re-reading the replacement there.
                Err(revert_error) => match read_link_in(&dir, &destination_name) {
                    Ok(raw) if raw == target => Ok(Some(format!(
                        "{observed} and could not be swapped back ({revert_error}); the \
                         replacement link is live and the arriving object is preserved here"
                    ))),
                    _ => Err(ReplaceLinkError::ResidualTemporary {
                        path: residual_path(),
                        detail: format!(
                            "{observed} and could not be swapped back ({revert_error}), and the \
                             destination no longer holds the replacement; the arriving object is \
                             preserved here"
                        ),
                    }),
                },
            }
        }
    };
    // Descriptor-relative writes land in the pinned directory wherever it
    // now lives, so success — and the receipt the caller records against the
    // planned pathname — may be claimed only while that pathname still names
    // the pinned directory. That classification belongs to the caller, at the
    // last moment before the receipt becomes durable, so the pin itself is
    // returned with the result.
    let residue = outcome?;
    Ok(LinkReplacement {
        pin: ReplacementPin {
            dir,
            base: base.to_path_buf(),
            planned_parent: parent.to_path_buf(),
            destination_file_name: destination_file_name.to_os_string(),
            temporary_file_name,
        },
        residue,
    })
}

/// The outcome of removing a temporary entry that must still hold a proven
/// object.
#[cfg(unix)]
enum TemporaryCleanup {
    /// The entry held the proven link (or was already gone) and no longer
    /// exists.
    Removed,
    /// The entry holds something other than the proven link; it was preserved
    /// and `observed` states what occupies it.
    NotProven { observed: String },
    /// The entry held the proven link, but removing it failed.
    RemoveFailed(io::Error),
}

/// Remove the entry named `name` in the pinned directory only when it is a
/// symbolic link whose raw target is byte-identical to `proven`; preserve
/// anything else.
///
/// The check and the unlink are still two syscalls — Linux and macOS offer no
/// unlink bound to the verified inode — so this narrows rather than closes
/// that window. What makes the remainder acceptable is the name it applies
/// to: both run against the pinned parent descriptor, so no rename of the
/// root can redirect them, and every caller passes a temporary name embedding
/// this process's id and a nanosecond timestamp, which no accidental writer
/// reaches. A process racing that private name deliberately holds the
/// home-directory write access `apply_install` records as the boundary of
/// what any pathname discipline can defend.
#[cfg(unix)]
fn remove_proven_temporary(
    dir: &fs::File,
    name: &std::ffi::CStr,
    proven: &Path,
) -> TemporaryCleanup {
    match read_link_in(dir, name) {
        Ok(raw) if raw == proven => match unlink_in(dir, name) {
            Ok(()) => TemporaryCleanup::Removed,
            Err(error) => TemporaryCleanup::RemoveFailed(error),
        },
        Ok(raw) => TemporaryCleanup::NotProven {
            observed: format!("a symbolic link to {} occupies", raw.display()),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => TemporaryCleanup::Removed,
        Err(_) => TemporaryCleanup::NotProven {
            observed: "an object that is not a symbolic link occupies".to_owned(),
        },
    }
}

/// The reparse tag Windows gives a name-surrogate symbolic link — the object
/// `std::os::windows::fs::symlink_dir` creates and the only object repair
/// ever placed at an installation path. A junction carries a different tag
/// and is refused as unproven.
#[cfg_attr(not(windows), allow(dead_code))]
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

/// The flag marking a symbolic link's substitute name as relative rather
/// than a full NT path.
#[cfg_attr(not(windows), allow(dead_code))]
const SYMLINK_FLAG_RELATIVE: u32 = 0x0000_0001;

/// Parse a `REPARSE_DATA_BUFFER` as a symbolic link's substitute name and
/// relative flag, exactly as stored.
///
/// Deliberately raw: `std::fs::read_link` converts the substitute name to a
/// user-facing spelling through `GetFullPathNameW` round-trips this parser
/// cannot reproduce, so the proof does not try to speak that dialect.
/// Instead both the stored name and the proven target are reduced through
/// [`comparable_symlink_target`] before they are compared.
///
/// Pure over bytes, and compiled on every platform, so the byte-layout
/// handling is pinned by tests that run where development happens rather
/// than only where the syscall does.
#[cfg_attr(not(windows), allow(dead_code))]
fn symlink_reparse_target(buffer: &[u8]) -> Result<(Vec<u16>, bool), &'static str> {
    const HEADER: usize = 8;
    const PATH_BUFFER: usize = 20;
    if buffer.len() < PATH_BUFFER {
        return Err("the reparse data is too short to be a symbolic link");
    }
    let field = |at: usize| u16::from_le_bytes([buffer[at], buffer[at + 1]]);
    let tag = u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]);
    if tag != IO_REPARSE_TAG_SYMLINK {
        return Err("the object is a reparse point but not a symbolic link");
    }
    let data_length = field(4) as usize;
    let end = HEADER
        .checked_add(data_length)
        .filter(|end| *end <= buffer.len())
        .ok_or("the reparse data states a length beyond what was read")?;
    let substitute_offset = field(8) as usize;
    let substitute_length = field(10) as usize;
    let flags = u32::from_le_bytes([buffer[16], buffer[17], buffer[18], buffer[19]]);
    if !substitute_length.is_multiple_of(2) || !substitute_offset.is_multiple_of(2) {
        return Err("the reparse data names a target on an odd byte boundary");
    }
    let start = PATH_BUFFER
        .checked_add(substitute_offset)
        .filter(|start| *start <= end)
        .ok_or("the reparse data places its target outside what was read")?;
    let stop = start
        .checked_add(substitute_length)
        .filter(|stop| *stop <= end)
        .ok_or("the reparse data places its target outside what was read")?;
    let (pairs, _remainder) = buffer[start..stop].as_chunks::<2>();
    let target: Vec<u16> = pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
    Ok((target, flags & SYMLINK_FLAG_RELATIVE != 0))
}

/// Reduce one spelling of a symbolic link target to the form the ownership
/// proof compares.
///
/// The same absolute target has several spellings across the two sides of
/// that comparison: the reparse data stores the NT form (`\??\C:\x`,
/// `\??\UNC\server\share`), while `std::fs::read_link` — which is what the
/// final recheck read and what receipts were recorded against — reports the
/// user form (`C:\x`, `\\server\share`), keeping the verbatim `\\?\` prefix
/// only where the path needs it. Stripping the NT or verbatim prefix and
/// rewriting a namespaced `UNC\` to `\\` maps every one of those spellings of
/// one target to one sequence, and touches nothing else: a relative name —
/// one literally starting `UNC\` included — carries no prefix and passes
/// through untouched, and every unit after the prefix is compared exactly,
/// trailing dots and spaces included, because the prefixes decide only which
/// namespace interprets those same units.
#[cfg_attr(not(windows), allow(dead_code))]
fn comparable_symlink_target(units: &[u16]) -> Vec<u16> {
    // `\??\` and `\\?\` in UTF-16.
    const NT_PREFIX: [u16; 4] = [92, 63, 63, 92];
    const VERBATIM_PREFIX: [u16; 4] = [92, 92, 63, 92];
    // `UNC\` in UTF-16.
    const UNC: [u16; 4] = [85, 78, 67, 92];
    let Some(rest) = units
        .strip_prefix(&NT_PREFIX)
        .or_else(|| units.strip_prefix(&VERBATIM_PREFIX))
    else {
        return units.to_vec();
    };
    if let Some(share) = rest.strip_prefix(&UNC) {
        let mut comparable = vec![92, 92];
        comparable.extend_from_slice(share);
        return comparable;
    }
    rest.to_vec()
}

/// Verify and delete the proven directory symbolic link through one handle.
///
/// This is the skilled-tdm closure of the Windows remove-and-create window.
/// `remove_dir` re-resolved the pathname and deleted whatever directory-like
/// object had arrived there — a stranger's directory symlink or an empty
/// directory included. Here the destination is opened once with
/// `FILE_FLAG_OPEN_REPARSE_POINT`, so the handle pins the link object itself
/// rather than what it points at; the reparse data is read through that
/// handle and must name the proven target byte for byte; and the deletion is
/// `SetFileInformationByHandle` with POSIX-semantics disposition on the same
/// handle, so exactly the verified object is removed whatever the pathname
/// has come to name meanwhile. A filesystem that refuses the POSIX-semantics
/// disposition refuses the repair rather than falling back to a pathname
/// delete — the same stance the Unix side takes on a filesystem that cannot
/// exchange.
///
/// The open itself is still pathname-resolved. An ancestor of the
/// destination renamed between the final recheck and this open re-resolves
/// the pathname through whatever now stands there, and a byte-identical
/// decoy link in a substituted root would be pinned, proven, and removed in
/// the stranger's directory, with the replacement then created beside it.
/// Reaching that requires the home-directory write access `apply_install`
/// records as the boundary of what any pathname discipline can defend, and
/// closing it needs an NT relative open bound to a pinned root handle —
/// tracked as `skilled-se9` rather than half-built here, because no
/// development or CI platform of this project can compile or exercise it.
///
/// What the proof establishes is byte-identity, not object identity: a
/// symbolic link substituted after the final recheck with the very same
/// target is accepted and removed. That is the equivalence the Unix
/// exchange applies to its displaced object for the same reason — a
/// directory link carries nothing but its target, so two links with
/// identical raw targets are interchangeable for every claim the plan
/// makes, and nothing an owner could lose distinguishes them. Everything
/// else — a different target, another object kind, another reparse tag — is
/// a stranger's object and is refused with it untouched.
#[cfg(windows)]
fn remove_proven_directory_symlink(
    link_path: &Path,
    proven_target: &Path,
) -> Result<(), ReplaceLinkError> {
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
        FILE_DISPOSITION_INFO_EX, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FileDispositionInfoEx,
        MAXIMUM_REPARSE_DATA_BUFFER_SIZE, SetFileInformationByHandle,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_GET_REPARSE_POINT;

    // Delete sharing is denied on purpose: Rust's default share mode would
    // let another process rename or unlink the pinned object between the
    // proof below and the disposition, and the disposition follows the
    // object — the proven link would then be deleted wherever the rename
    // took it, outside the path the plan showed. Without delete sharing a
    // concurrent rename or delete fails while this handle is open, which is
    // the Windows spelling of the name binding the Unix exchange gets from
    // its pinned parent descriptor. An object someone else already holds
    // with conflicting access fails this open instead, and an unopenable
    // destination is a refusal.
    let file = fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | DELETE)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(link_path)
        .map_err(ReplaceLinkError::Unchanged)?;
    let metadata = file.metadata().map_err(ReplaceLinkError::Unchanged)?;
    // Both attributes, not the reparse point alone: a *file* symbolic link
    // carries the same reparse tag and can carry the same target bytes, and
    // the object repair proved and receipted is a directory link.
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
        || metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY == 0
    {
        return Err(ReplaceLinkError::ConcurrentlyReplaced {
            observed: "an object that is not a directory symbolic link occupies the destination"
                .to_owned(),
        });
    }
    let mut buffer = vec![0_u8; MAXIMUM_REPARSE_DATA_BUFFER_SIZE as usize];
    let mut returned: u32 = 0;
    // SAFETY: the handle is open, the buffer outlives the call, and the
    // returned length is bounded by the buffer length handed in.
    let read = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_GET_REPARSE_POINT,
            std::ptr::null(),
            0,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if read == 0 {
        return Err(ReplaceLinkError::Unchanged(io::Error::last_os_error()));
    }
    buffer.truncate(returned as usize);
    let (target, target_is_relative) = symlink_reparse_target(&buffer).map_err(|reason| {
        ReplaceLinkError::ConcurrentlyReplaced {
            observed: reason.to_owned(),
        }
    })?;
    let proven: Vec<u16> = proven_target.as_os_str().encode_wide().collect();
    // The relative flag has to agree with the proven target's own shape
    // before the names are compared: the flag decides how the stored units
    // resolve, so the same units under the other flag are a different link.
    if target_is_relative == proven_target.is_absolute()
        || comparable_symlink_target(&target) != comparable_symlink_target(&proven)
    {
        return Err(ReplaceLinkError::ConcurrentlyReplaced {
            observed: format!(
                "a symbolic link to {} occupies the destination",
                String::from_utf16_lossy(&target)
            ),
        });
    }
    let disposition = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    };
    // SAFETY: the handle is open and the structure matches the class named.
    let marked = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfoEx,
            (&disposition as *const FILE_DISPOSITION_INFO_EX).cast(),
            size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    };
    if marked == 0 {
        return Err(ReplaceLinkError::Unchanged(io::Error::last_os_error()));
    }
    drop(file);
    Ok(())
}

#[cfg(windows)]
fn replace_directory_symlink(
    target: &Path,
    link_path: &Path,
    proven_target: &Path,
    _base: &Path,
) -> Result<LinkReplacement, ReplaceLinkError> {
    remove_proven_directory_symlink(link_path, proven_target)?;
    create_directory_symlink(target, link_path)
        .map(|()| LinkReplacement { residue: None })
        .map_err(ReplaceLinkError::RemovedOldLink)
}

#[cfg(not(any(unix, windows)))]
fn replace_directory_symlink(
    _target: &Path,
    _link_path: &Path,
    _proven_target: &Path,
    _base: &Path,
) -> Result<LinkReplacement, ReplaceLinkError> {
    Err(ReplaceLinkError::Unchanged(io::Error::new(
        io::ErrorKind::Unsupported,
        "this platform does not support Skilled's directory-link repair",
    )))
}

#[derive(Debug)]
enum ReplaceLinkError {
    /// The destination still names the same object it did before this attempt.
    Unchanged(io::Error),
    /// The destination's filesystem or kernel cannot exchange paths
    /// atomically, so a provable replacement is impossible; nothing was
    /// written.
    #[cfg(unix)]
    ExchangeUnsupported(io::Error),
    /// An object that is not the proven link arrived at the destination after
    /// the final recheck. On Unix it was swapped back intact; on Windows it
    /// was never touched, because the handle-bound proof refused before any
    /// disposition was set. Nothing was written either way.
    #[cfg(any(unix, windows))]
    ConcurrentlyReplaced { observed: String },
    /// The proven old link was removed before replacement creation failed.
    #[cfg(windows)]
    RemovedOldLink(io::Error),
    /// No replacement is claimed at the destination — it is unchanged, or a
    /// failed revert left its state unproven — and an entry remains at a
    /// temporary path; `detail` states exactly what and where.
    #[cfg(unix)]
    ResidualTemporary { path: PathBuf, detail: String },
}

/// Check a repaired link and its OpenCode outlook against a fresh scan.
pub fn verify_repair(
    plan: &RepairPlan,
    applied: &RepairApplyReport,
    snapshot: &InventorySnapshot,
) -> VerifyReport {
    let mut held = Vec::new();
    let mut failures = Vec::new();
    let mut withheld = Vec::new();
    let Some(step) = applied.step.as_ref() else {
        return VerifyReport {
            held,
            failures,
            withheld,
        };
    };
    if !step.outcome.wrote_link() {
        let reason = match &step.outcome {
            RepairStepOutcome::RemovedUnreplaced(_) => {
                "the original link was removed but no replacement was written, so there was no repaired link to verify"
            }
            RepairStepOutcome::ResidualTemporary { path, .. } => {
                withheld.push(VerifyWithheld {
                    agent: step.agent,
                    postcondition: Postcondition::LinkAsPlanned,
                    reason: format!(
                        "the failed repair left an object preserved at {}",
                        path.display()
                    ),
                    required: true,
                    precluded_by_selection: false,
                });
                return VerifyReport {
                    held,
                    failures,
                    withheld,
                };
            }
            RepairStepOutcome::MovedRootUnreceipted { path, .. } => {
                withheld.push(VerifyWithheld {
                    agent: step.agent,
                    postcondition: Postcondition::LinkAsPlanned,
                    reason: format!(
                        "the skill root moved during the repair; the live replacement was written \
                         at {} without a receipt",
                        path.display()
                    ),
                    required: true,
                    precluded_by_selection: false,
                });
                return VerifyReport {
                    held,
                    failures,
                    withheld,
                };
            }
            _ => "the repair was not applied, so there was no repaired link to verify",
        };
        withheld.push(VerifyWithheld {
            agent: step.agent,
            postcondition: Postcondition::LinkAsPlanned,
            reason: reason.to_owned(),
            required: true,
            precluded_by_selection: false,
        });
        return VerifyReport {
            held,
            failures,
            withheld,
        };
    }
    if let Some(reason) = unscanned(snapshot.root(step.agent).status()) {
        withheld.push(VerifyWithheld {
            agent: step.agent,
            postcondition: Postcondition::LinkAsPlanned,
            reason,
            required: true,
            precluded_by_selection: false,
        });
        return VerifyReport {
            held,
            failures,
            withheld,
        };
    }
    let row = snapshot.row(plan.skill_name());
    let Some(observed) = row.and_then(|row| row.observation(step.agent)) else {
        failures.push(VerifyFailure {
            agent: step.agent,
            postcondition: Postcondition::LinkAsPlanned,
            observed: "the scan taken afterwards found nothing at this path".to_owned(),
        });
        return VerifyReport {
            held,
            failures,
            withheld,
        };
    };
    let Some(variant) = plan.variant() else {
        failures.push(VerifyFailure {
            agent: step.agent,
            postcondition: Postcondition::LinkAsPlanned,
            observed: "the repair plan carried no registered variant identity".to_owned(),
        });
        return VerifyReport {
            held,
            failures,
            withheld,
        };
    };
    let Some(expected_target) = plan.new_target() else {
        failures.push(VerifyFailure {
            agent: step.agent,
            postcondition: Postcondition::LinkAsPlanned,
            observed: "the repair plan carried no target path".to_owned(),
        });
        return VerifyReport {
            held,
            failures,
            withheld,
        };
    };
    match observed.object() {
        InstallationObject::Symlink { target } if target == expected_target => {}
        InstallationObject::Symlink { target } => {
            failures.push(VerifyFailure {
                agent: step.agent,
                postcondition: Postcondition::LinkAsPlanned,
                observed: format!(
                    "the symbolic link records {} instead of the planned target {}",
                    target.display(),
                    expected_target.display()
                ),
            });
            return VerifyReport {
                held,
                failures,
                withheld,
            };
        }
        _ => {}
    }
    match mismatch_variant(variant, observed) {
        Checked::Held => held.push(VerifyPass {
            agent: step.agent,
            postcondition: Postcondition::LinkAsPlanned,
        }),
        Checked::Failed(observed) => failures.push(VerifyFailure {
            agent: step.agent,
            postcondition: Postcondition::LinkAsPlanned,
            observed,
        }),
        Checked::Withheld(reason) => withheld.push(VerifyWithheld {
            agent: step.agent,
            postcondition: Postcondition::LinkAsPlanned,
            reason,
            required: true,
            precluded_by_selection: false,
        }),
    }
    if failures.is_empty() && !withheld.iter().any(VerifyWithheld::blocks_success) {
        match row.and_then(InventoryRow::opencode_resolution) {
            Some(OpenCodeResolution::Incomplete { roots }) => withheld.push(VerifyWithheld {
                agent: AgentKind::OpenCode,
                postcondition: Postcondition::OpenCodeResolution,
                reason: format!(
                    "what OpenCode resolves the name to could not be established: {}",
                    unknown_roots(roots)
                ),
                required: false,
                precluded_by_selection: unknown_only_by_selection(roots, snapshot),
            }),
            Some(resolution) => {
                let actual = OpenCodeOutlook::of(resolution);
                match plan.opencode_outlook() {
                    Some(expected) if expected != &actual => failures.push(VerifyFailure {
                        agent: AgentKind::OpenCode,
                        postcondition: Postcondition::OpenCodeResolution,
                        observed: format!(
                            "this was not what the plan described: {}",
                            observed_summary(resolution)
                        ),
                    }),
                    Some(_) => held.push(VerifyPass {
                        agent: AgentKind::OpenCode,
                        postcondition: Postcondition::OpenCodeResolution,
                    }),
                    None => {}
                }
            }
            None if plan.opencode_outlook().is_some() => withheld.push(VerifyWithheld {
                agent: AgentKind::OpenCode,
                postcondition: Postcondition::OpenCodeResolution,
                reason: "the fresh scan produced no OpenCode resolution, so that ancillary postcondition was not checked"
                    .to_owned(),
                required: false,
                // The scan computes no resolution at all exactly when OpenCode
                // itself was deselected; anything else leaves an Incomplete
                // resolution rather than none. Deselection alone is not enough
                // to call the gap the user's choice, though: a selected root
                // the scan could not read would have kept this check from
                // running just as surely, and must not hide behind it.
                precluded_by_selection: scan_gaps_only_by_selection(row, snapshot),
            }),
            None => {}
        }
    }
    VerifyReport {
        held,
        failures,
        withheld,
    }
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
            Some(RepairStepOutcome::RepairedResidualTemporary { .. }) => {
                RepairStatus::PartiallyApplied
            }
            Some(RepairStepOutcome::MovedRootUnreceipted { .. }) => RepairStatus::PartiallyApplied,
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

/// Remove only links that still satisfy every ownership and containment guard.
///
/// The final component is never followed and the documented root is never
/// removed. As with installation, one pathname window remains between the
/// final check and the write because the standard library has no handle-based
/// `unlinkat`; the mandatory rescan makes any disagreement non-silent.
pub(crate) fn apply_uninstall(plan: &UninstallPlan, store: &Store, home: &Path) -> ApplyReport {
    let mut steps = Vec::new();
    if plan.is_blocked() {
        debug_assert!(false, "a blocked plan reached apply_uninstall");
        return ApplyReport { steps };
    }
    let mut stopped = false;
    for target in plan.targets.iter().filter(|target| target.is_work()) {
        let outcome = if stopped {
            StepOutcome::Unattempted
        } else {
            apply_uninstall_target(plan, target, store, home)
        };
        stopped = !matches!(outcome, StepOutcome::Removed);
        steps.push(AppliedStep {
            agent: target.agent,
            link_path: target.link_path.clone(),
            outcome,
        });
    }
    ApplyReport { steps }
}

fn apply_uninstall_target(
    plan: &UninstallPlan,
    target: &UninstallTarget,
    store: &Store,
    home: &Path,
) -> StepOutcome {
    let UninstallDisposition::RemoveLink { link_target, .. } = &target.disposition else {
        return StepOutcome::Failed("the target is not removable".to_owned());
    };
    if !target.link_path.is_absolute()
        || target.link_path.file_name() != Some(std::ffi::OsStr::new(plan.skill_name()))
    {
        return StepOutcome::Failed(
            "the target is not beneath the documented skill root".to_owned(),
        );
    }
    let Some(root) = target.link_path.parent() else {
        return StepOutcome::Failed("the target has no parent directory".to_owned());
    };
    let documented_root = home.join(adapter(target.agent).native_skill_root());
    if root != documented_root {
        return StepOutcome::Failed(
            "the target is not beneath the agent's exact documented skill root".to_owned(),
        );
    }
    if probe_root(root, home) != RootProbe::Present {
        return StepOutcome::Failed(
            "the agent's skill root changed after the plan was shown, so nothing was removed"
                .to_owned(),
        );
    }
    match fs::symlink_metadata(&target.link_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {}
        _ => return StepOutcome::Failed(
            "the path is no longer the symbolic link the plan described, so nothing was removed"
                .to_owned(),
        ),
    }
    let receipts = match store.receipts() {
        Ok(receipts) => receipts,
        Err(error) => {
            return StepOutcome::Failed(format!(
                "the ownership receipts could not be re-read, so nothing was removed: {error}"
            ));
        }
    };
    if !receipts.iter().any(|receipt| {
        receipt.agent == target.agent
            && receipt.link_path == target.link_path
            && receipt.link_target == *link_target
    }) {
        return StepOutcome::Failed(
            "the matching ownership receipt disappeared after the plan was shown, so nothing was removed"
                .to_owned(),
        );
    }
    match fs::read_link(&target.link_path) {
        Ok(current) if current == *link_target => {}
        Ok(_) => {
            return StepOutcome::Failed(
                "the link target changed after the plan was shown, so nothing was removed"
                    .to_owned(),
            );
        }
        Err(error) => {
            return StepOutcome::Failed(format!(
                "the symbolic link target could not be re-read, so nothing was removed: {error}"
            ));
        }
    }
    match remove_directory_symlink(&target.link_path) {
        Ok(()) => StepOutcome::Removed,
        Err(error) => {
            StepOutcome::Failed(format!("the managed link could not be removed: {error}"))
        }
    }
}

#[cfg(unix)]
fn remove_directory_symlink(path: &Path) -> io::Result<()> {
    fs::remove_file(path)
}

#[cfg(windows)]
fn remove_directory_symlink(path: &Path) -> io::Result<()> {
    fs::remove_dir(path)
}

#[cfg(not(any(unix, windows)))]
fn remove_directory_symlink(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Skilled cannot remove directory symbolic links on this platform",
    ))
}

/// What a direct post-removal read of canonical content found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContentSighting {
    Resolved,
    Missing,
    Unreadable(String),
    NotApplicable,
}

pub(crate) fn probe_uninstall_content(plan: &UninstallPlan) -> [ContentSighting; 3] {
    AgentKind::ALL.map(|agent| {
        let Some(target) = plan.target(agent) else {
            return ContentSighting::NotApplicable;
        };
        let UninstallDisposition::RemoveLink {
            link_target,
            target_state: UninstallTargetState::Directory,
            ..
        } = target.disposition()
        else {
            return ContentSighting::NotApplicable;
        };
        match fs::metadata(link_target) {
            Ok(metadata) if metadata.is_dir() => ContentSighting::Resolved,
            Ok(_) => ContentSighting::Missing,
            Err(error) if error.kind() == io::ErrorKind::NotFound => ContentSighting::Missing,
            Err(error) => ContentSighting::Unreadable(error.to_string()),
        }
    })
}

/// Verify link removal, canonical-content survival, and the reduced OpenCode outlook.
pub fn verify_uninstall(
    plan: &UninstallPlan,
    applied: &ApplyReport,
    snapshot: &InventorySnapshot,
    content: &[ContentSighting; 3],
) -> VerifyReport {
    let mut report = VerifyReport::default();
    let row = snapshot.row(plan.skill_name());
    for step in applied.steps.iter().filter(|step| step.removed_link()) {
        if let Some(reason) = unscanned(snapshot.root(step.agent).status()) {
            report.withheld.push(VerifyWithheld {
                agent: step.agent,
                postcondition: Postcondition::LinkGone,
                reason,
                required: true,
                precluded_by_selection: false,
            });
        } else if row.and_then(|row| row.observation(step.agent)).is_none() {
            report.held.push(VerifyPass {
                agent: step.agent,
                postcondition: Postcondition::LinkGone,
            });
        } else {
            report.failures.push(VerifyFailure {
                agent: step.agent,
                postcondition: Postcondition::LinkGone,
                observed: "the scan taken afterwards still found an entry at this path".to_owned(),
            });
        }
        let target = plan
            .target(step.agent)
            .expect("an applied target belongs to the plan");
        match &target.disposition {
            UninstallDisposition::RemoveLink {
                target_state: UninstallTargetState::Directory,
                ..
            } => match &content[step.agent.index()] {
                ContentSighting::Resolved => report.held.push(VerifyPass {
                    agent: step.agent,
                    postcondition: Postcondition::ContentSurvived,
                }),
                ContentSighting::Missing => report.failures.push(VerifyFailure {
                    agent: step.agent,
                    postcondition: Postcondition::ContentSurvived,
                    observed: "the recorded target content is no longer present; Skilled removed only the link"
                        .to_owned(),
                }),
                ContentSighting::Unreadable(reason) => report.withheld.push(VerifyWithheld {
                    agent: step.agent,
                    postcondition: Postcondition::ContentSurvived,
                    reason: format!("the recorded target content could not be re-read: {reason}"),
                    required: false,
                    precluded_by_selection: false,
                }),
                ContentSighting::NotApplicable => report.withheld.push(VerifyWithheld {
                    agent: step.agent,
                    postcondition: Postcondition::ContentSurvived,
                    reason: "the recorded target content was not re-read".to_owned(),
                    required: false,
                    precluded_by_selection: false,
                }),
            },
            UninstallDisposition::RemoveLink {
                target_state: UninstallTargetState::Unreadable(reason),
                ..
            } => report.withheld.push(VerifyWithheld {
                agent: step.agent,
                postcondition: Postcondition::ContentSurvived,
                reason: format!(
                    "the recorded target content was unreadable before link removal: {reason}"
                ),
                required: false,
                precluded_by_selection: false,
            }),
            _ => {}
        }
    }
    if applied.steps.iter().any(AppliedStep::removed_link)
        && let Some(expected) = plan.opencode_outlook()
    {
        let observed = row.and_then(InventoryRow::opencode_resolution);
        if matches!(expected, UninstallOutlook::Unknown) {
            report.withheld.push(VerifyWithheld {
                agent: AgentKind::OpenCode,
                postcondition: Postcondition::OpenCodeResolution,
                reason:
                    "the plan could not establish what OpenCode would resolve after the uninstall"
                        .to_owned(),
                required: false,
                precluded_by_selection: false,
            });
        } else if let Some(observed) = observed {
            match UninstallOutlook::of(observed) {
                UninstallOutlook::Unknown => report.withheld.push(VerifyWithheld {
                    agent: AgentKind::OpenCode,
                    postcondition: Postcondition::OpenCodeResolution,
                    reason: "what OpenCode resolves the name to could not be established"
                        .to_owned(),
                    required: false,
                    precluded_by_selection: match observed {
                        OpenCodeResolution::Incomplete { roots } => {
                            unknown_only_by_selection(roots, snapshot)
                        }
                        _ => false,
                    },
                }),
                actual if &actual == expected => report.held.push(VerifyPass {
                    agent: AgentKind::OpenCode,
                    postcondition: Postcondition::OpenCodeResolution,
                }),
                _ => report.failures.push(VerifyFailure {
                    agent: AgentKind::OpenCode,
                    postcondition: Postcondition::OpenCodeResolution,
                    observed: format!(
                        "this was not the OpenCode outcome the plan described: {}",
                        observed_summary(observed)
                    ),
                }),
            }
        } else if row.is_none() {
            let gaps = AgentKind::ALL
                .into_iter()
                .filter_map(|agent| {
                    unscanned(snapshot.root(agent).status())
                        .map(|reason| format!("{}: {reason}", agent.display_name()))
                })
                .collect::<Vec<_>>();
            if gaps.is_empty() && matches!(expected, UninstallOutlook::Nothing) {
                report.held.push(VerifyPass {
                    agent: AgentKind::OpenCode,
                    postcondition: Postcondition::OpenCodeResolution,
                });
            } else if gaps.is_empty() {
                report.failures.push(VerifyFailure {
                    agent: AgentKind::OpenCode,
                    postcondition: Postcondition::OpenCodeResolution,
                    observed: "OpenCode found nothing under this name".to_owned(),
                });
            } else {
                report.withheld.push(VerifyWithheld {
                    agent: AgentKind::OpenCode,
                    postcondition: Postcondition::OpenCodeResolution,
                    reason: format!(
                        "what OpenCode resolves the name to could not be established: {}",
                        gaps.join("; ")
                    ),
                    required: false,
                    precluded_by_selection: scan_gaps_only_by_selection(row, snapshot),
                });
            }
        } else {
            report.withheld.push(VerifyWithheld {
                agent: AgentKind::OpenCode,
                postcondition: Postcondition::OpenCodeResolution,
                reason: "the post-uninstall inventory did not state OpenCode's resolution"
                    .to_owned(),
                required: false,
                precluded_by_selection: false,
            });
        }
    }
    report
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizeFailure {
    agent: AgentKind,
    reason: String,
    invalidates_verification: bool,
}

impl FinalizeFailure {
    pub fn agent(&self) -> AgentKind {
        self.agent
    }
    pub fn reason(&self) -> &str {
        &self.reason
    }
    fn invalidates_verification(&self) -> bool {
        self.invalidates_verification
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FinalizeReport {
    failures: Vec<FinalizeFailure>,
}

impl FinalizeReport {
    pub fn failures(&self) -> &[FinalizeFailure] {
        &self.failures
    }
}

/// Delete receipts only after a positive LinkGone pass authorizes it.
///
/// Content survival remains an independently reported postcondition, but it
/// cannot make an already-gone link's inert receipt useful again. The final
/// link-path recheck and receipt deletion share one metadata mutation guard,
/// so another Skilled process cannot commit a replacement receipt in between.
pub(crate) fn finalize_uninstall(
    plan: &UninstallPlan,
    applied: &ApplyReport,
    verification: &VerifyReport,
    store: &mut Store,
) -> FinalizeReport {
    let mut report = FinalizeReport::default();
    for step in applied.steps.iter().filter(|step| step.removed_link()) {
        if !verification_holds(verification, step.agent, Postcondition::LinkGone) {
            report.failures.push(FinalizeFailure {
                agent: step.agent,
                reason: "the ownership receipt was retained because link removal was not positively verified"
                    .to_owned(),
                invalidates_verification: true,
            });
            continue;
        }
        let Some(target) = plan.target(step.agent) else {
            continue;
        };
        let UninstallDisposition::RemoveLink { link_target, .. } = target.disposition() else {
            continue;
        };
        let mutation = match store.begin_mutation() {
            Ok(mutation) => mutation,
            Err(error) => {
                report.failures.push(FinalizeFailure {
                    agent: step.agent,
                    reason: format!(
                        "the ownership receipt was retained because the metadata mutation guard could not be acquired: {error}"
                    ),
                    invalidates_verification: true,
                });
                continue;
            }
        };
        match exact_link_is_inactive(step.link_path(), link_target) {
            Ok(true) => {}
            Ok(false) => {
                report.failures.push(FinalizeFailure {
                    agent: step.agent,
                    reason: "the ownership receipt was retained because the managed link became active again after verification"
                        .to_owned(),
                    invalidates_verification: true,
                });
                continue;
            }
            Err(error) => {
                report.failures.push(FinalizeFailure {
                    agent: step.agent,
                    reason: format!(
                        "the ownership receipt was retained because the link path could not be re-read: {error}"
                    ),
                    invalidates_verification: true,
                });
                continue;
            }
        }
        if let Err(error) =
            mutation.delete_receipts_for_link(step.agent, step.link_path(), link_target)
        {
            report.failures.push(FinalizeFailure {
                agent: step.agent,
                reason: error.to_string(),
                invalidates_verification: false,
            });
            continue;
        }
        if let Err(error) = mutation.commit() {
            report.failures.push(FinalizeFailure {
                agent: step.agent,
                reason: error.to_string(),
                invalidates_verification: false,
            });
        }
    }
    report
}

fn verification_holds(
    verification: &VerifyReport,
    agent: AgentKind,
    postcondition: Postcondition,
) -> bool {
    verification
        .held
        .iter()
        .any(|pass| pass.agent == agent && pass.postcondition == postcondition)
}

/// Re-read the final component immediately before receipt deletion.
///
/// A different occupant means the exact link described by the receipt is gone;
/// an exact matching symbolic link means it became active again and its
/// ownership evidence must survive.
fn exact_link_is_inactive(link_path: &Path, link_target: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(link_path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error),
        Ok(metadata) if !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => fs::read_link(link_path).map(|target| target != link_target),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UninstallStatus {
    NothingToDo,
    NotApplied,
    PartiallyApplied,
    VerificationFailed,
    UninstalledUnrecorded,
    Uninstalled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UninstallOutcome {
    plan: UninstallPlan,
    applied: ApplyReport,
    verification: VerifyReport,
    finalized: FinalizeReport,
}

impl UninstallOutcome {
    pub(crate) fn new(
        plan: UninstallPlan,
        applied: ApplyReport,
        verification: VerifyReport,
        finalized: FinalizeReport,
    ) -> Self {
        Self {
            plan,
            applied,
            verification,
            finalized,
        }
    }
    pub fn plan(&self) -> &UninstallPlan {
        &self.plan
    }
    pub fn applied(&self) -> &ApplyReport {
        &self.applied
    }
    pub fn verification(&self) -> &VerifyReport {
        &self.verification
    }
    pub fn finalized(&self) -> &FinalizeReport {
        &self.finalized
    }
    pub fn status(&self) -> UninstallStatus {
        uninstall_status(&self.applied, &self.verification, &self.finalized)
    }
}

fn uninstall_status(
    applied: &ApplyReport,
    verification: &VerifyReport,
    finalized: &FinalizeReport,
) -> UninstallStatus {
    if applied.steps.is_empty() {
        return UninstallStatus::NothingToDo;
    }
    if !applied.steps.iter().all(AppliedStep::removed_link) {
        return if applied.steps.iter().any(AppliedStep::removed_link) {
            UninstallStatus::PartiallyApplied
        } else {
            UninstallStatus::NotApplied
        };
    }
    if !verification.is_verified()
        || finalized
            .failures
            .iter()
            .any(FinalizeFailure::invalidates_verification)
    {
        return UninstallStatus::VerificationFailed;
    }
    if !finalized.failures.is_empty() {
        return UninstallStatus::UninstalledUnrecorded;
    }
    UninstallStatus::Uninstalled
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UninstallPrompt {
    Preview(UninstallPlan),
    Report(UninstallOutcome),
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ForgetObservation {
    Active,
    Inactive(String),
    Unreadable(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForgetProbe {
    observations: Vec<ForgetObservation>,
}

/// Inspect every receipted path without following its final component.
pub fn probe_forget(source: &RegisteredSource, receipts: &[Receipt]) -> ForgetProbe {
    let observations = receipts
        .iter()
        .filter(|receipt| receipt.source_id == Some(source.id()))
        .map(|receipt| match fs::symlink_metadata(receipt.link_path()) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                ForgetObservation::Inactive("nothing remains at this path".to_owned())
            }
            Err(error) => ForgetObservation::Unreadable(error.to_string()),
            Ok(metadata) if !metadata.file_type().is_symlink() => {
                ForgetObservation::Inactive("the path is no longer a symbolic link".to_owned())
            }
            Ok(_) => match fs::read_link(receipt.link_path()) {
                Ok(target) if target == receipt.link_target() => ForgetObservation::Active,
                Ok(target) => ForgetObservation::Inactive(format!(
                    "the link now points to {}",
                    target.display()
                )),
                Err(error) => ForgetObservation::Unreadable(error.to_string()),
            },
        })
        .collect();
    ForgetProbe { observations }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ForgetReceiptState {
    Active,
    Inactive { reason: String },
    Unreadable { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForgetReceipt {
    receipt: Receipt,
    state: ForgetReceiptState,
}

impl ForgetReceipt {
    pub fn receipt(&self) -> &Receipt {
        &self.receipt
    }
    pub fn state(&self) -> &ForgetReceiptState {
        &self.state
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForgetPlan {
    source: RegisteredSource,
    receipts: Vec<ForgetReceipt>,
    blocking_findings: Vec<Finding>,
}

impl ForgetPlan {
    pub fn source(&self) -> &RegisteredSource {
        &self.source
    }
    pub fn receipts(&self) -> &[ForgetReceipt] {
        &self.receipts
    }
    pub fn blocking_findings(&self) -> &[Finding] {
        &self.blocking_findings
    }
    pub fn is_blocked(&self) -> bool {
        !self.blocking_findings.is_empty()
    }
    pub fn is_executable(&self) -> bool {
        !self.is_blocked()
    }
}

/// Classify exactly which inactive ownership facts will be discarded.
pub fn plan_forget(
    source: &RegisteredSource,
    receipts: &[Receipt],
    probe: &ForgetProbe,
) -> ForgetPlan {
    let source_receipts: Vec<Receipt> = receipts
        .iter()
        .filter(|receipt| receipt.source_id == Some(source.id()))
        .cloned()
        .collect();
    let mut blocking_findings = Vec::new();
    let classified = source_receipts
        .into_iter()
        .zip(&probe.observations)
        .map(|(receipt, observation)| {
            let state = match observation {
                ForgetObservation::Active => {
                    blocking_findings.push(Finding::new(
                        "forget.active_links",
                        FindingSeverity::Critical,
                        format!(
                            "an active managed link remains at {}",
                            receipt.link_path().display()
                        ),
                    ));
                    ForgetReceiptState::Active
                }
                ForgetObservation::Inactive(reason) => ForgetReceiptState::Inactive {
                    reason: reason.clone(),
                },
                ForgetObservation::Unreadable(reason) => {
                    blocking_findings.push(Finding::new(
                        "forget.unreadable_link",
                        FindingSeverity::Critical,
                        format!(
                            "{} could not be established inactive: {reason}",
                            receipt.link_path().display()
                        ),
                    ));
                    ForgetReceiptState::Unreadable {
                        reason: reason.clone(),
                    }
                }
            };
            ForgetReceipt { receipt, state }
        })
        .collect();
    ForgetPlan {
        source: source.clone(),
        receipts: classified,
        blocking_findings,
    }
}

/// A receipt-table failure is a safety block, not an absent receipt set.
pub fn plan_forget_unreadable_receipts(source: &RegisteredSource, reason: String) -> ForgetPlan {
    ForgetPlan {
        source: source.clone(),
        receipts: Vec::new(),
        blocking_findings: vec![Finding::new(
            "forget.unreadable_receipts",
            FindingSeverity::Critical,
            format!(
                "ownership receipts could not be read, so Skilled cannot establish that every link is inactive: {reason}"
            ),
        )],
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ForgetApply {
    NothingToDo,
    Forgotten,
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ForgetVerification {
    Held,
    Failed(String),
    Withheld(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForgetStatus {
    NothingToDo,
    NotForgotten,
    Forgotten,
    VerificationFailed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForgetOutcome {
    plan: ForgetPlan,
    applied: ForgetApply,
    verification: ForgetVerification,
}

impl ForgetOutcome {
    pub(crate) fn new(
        plan: ForgetPlan,
        applied: ForgetApply,
        verification: ForgetVerification,
    ) -> Self {
        Self {
            plan,
            applied,
            verification,
        }
    }
    pub fn plan(&self) -> &ForgetPlan {
        &self.plan
    }
    pub fn applied(&self) -> &ForgetApply {
        &self.applied
    }
    pub fn verification(&self) -> &ForgetVerification {
        &self.verification
    }
    pub fn status(&self) -> ForgetStatus {
        match (&self.applied, &self.verification) {
            (ForgetApply::NothingToDo, _) => ForgetStatus::NothingToDo,
            (ForgetApply::Failed(_), _) => ForgetStatus::NotForgotten,
            (ForgetApply::Forgotten, ForgetVerification::Held) => ForgetStatus::Forgotten,
            (ForgetApply::Forgotten, _) => ForgetStatus::VerificationFailed,
        }
    }
}

/// Recheck the exact receipt multiset and every link immediately before deletion.
///
/// The mutation guard begins before both checks and stays held through the
/// transaction commit. Install and repair — the two operations that make a link
/// active and record ownership of it — acquire that same guard before they
/// write, so no other Skilled process can make a receipt active inside this
/// window.
pub(crate) fn apply_forget(plan: &ForgetPlan, store: &mut Store) -> ForgetApply {
    if plan.is_blocked() {
        debug_assert!(false, "a blocked plan reached apply_forget");
        return ForgetApply::Failed("the plan is blocked".to_owned());
    }
    let mutation = match store.begin_mutation() {
        Ok(mutation) => mutation,
        Err(error) => {
            return ForgetApply::Failed(format!(
                "the metadata mutation guard could not be acquired: {error}"
            ));
        }
    };
    let expected: Vec<Receipt> = plan
        .receipts
        .iter()
        .map(|item| item.receipt.clone())
        .collect();
    let current = match mutation.receipts() {
        Ok(receipts) => receipts
            .into_iter()
            .filter(|receipt| receipt.source_id == Some(plan.source.id()))
            .collect::<Vec<_>>(),
        Err(error) => {
            return ForgetApply::Failed(format!(
                "the ownership receipts could not be re-read: {error}"
            ));
        }
    };
    if current != expected {
        return ForgetApply::Failed(
            "the source's receipt set changed after the preview was shown".to_owned(),
        );
    }
    match mutation.source_is_registered(plan.source.id()) {
        Ok(false) if current.is_empty() => {
            return match mutation.commit() {
                Ok(()) => ForgetApply::NothingToDo,
                Err(error) => {
                    ForgetApply::Failed(format!("the metadata transaction failed: {error}"))
                }
            };
        }
        Ok(false) => {
            return ForgetApply::Failed(
                "the source disappeared while ownership receipts still remain".to_owned(),
            );
        }
        Ok(true) => {}
        Err(error) => {
            return ForgetApply::Failed(format!(
                "the source metadata could not be re-read: {error}"
            ));
        }
    }
    match mutation.source_matches(&plan.source) {
        Ok(true) => {}
        Ok(false) => {
            return ForgetApply::Failed(
                "the source or catalog metadata changed after the preview was shown".to_owned(),
            );
        }
        Err(error) => {
            return ForgetApply::Failed(format!(
                "the source and catalog metadata could not be re-read: {error}"
            ));
        }
    }
    let reprobe = probe_forget(&plan.source, &current);
    if reprobe
        .observations
        .iter()
        .any(|observation| !matches!(observation, ForgetObservation::Inactive(_)))
    {
        return ForgetApply::Failed(
            "a receipted path is active or unreadable now, so no metadata was removed".to_owned(),
        );
    }
    let deleted = match mutation.forget_source(plan.source.id()) {
        Ok(deleted) => deleted,
        Err(error) => {
            return ForgetApply::Failed(format!("the metadata transaction failed: {error}"));
        }
    };
    if let Err(error) = mutation.commit() {
        return ForgetApply::Failed(format!("the metadata transaction failed: {error}"));
    }
    if deleted == 0 {
        ForgetApply::NothingToDo
    } else {
        ForgetApply::Forgotten
    }
}

/// Check both of Forget's postconditions: the metadata is gone, and the
/// checkout is not.
///
/// The preview states the second one as plainly as the first — it names the
/// checkout path and promises it is left alone — so a report that says
/// "forgotten and verified" without looking at that path would be claiming
/// something nothing checked.
///
/// A directory being there is not the checkout being there, and this is the one
/// place that distinction has to be paid for: an empty replacement at the same
/// pathname would satisfy an object-type check while the repository it stood
/// for is gone. The registered revision is the same identity witness install
/// and repair use, so the check is that the path still canonicalizes to itself
/// and that the repository there still contains the revision the source was
/// registered at. What the operating system or Git will not answer about leaves
/// the check withheld, which is the inventory's own rule applied here.
pub(crate) fn verify_forget(plan: &ForgetPlan, store: &Store) -> ForgetVerification {
    match store.verify_source_forgotten(plan.source.id()) {
        Ok([true, true, true]) => {}
        Ok(checks) => {
            return ForgetVerification::Failed(format!(
                "metadata remained after forgetting (source: {}, catalogs: {}, receipts: {})",
                !checks[0], !checks[1], !checks[2],
            ));
        }
        Err(error) => {
            return ForgetVerification::Withheld(format!(
                "the metadata could not be re-read: {error}"
            ));
        }
    }
    let checkout = plan.source.git_top_level();
    match fs::symlink_metadata(checkout) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return ForgetVerification::Failed(format!(
                "the checkout at {} is no longer a directory",
                checkout.display()
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return ForgetVerification::Failed(format!(
                "the checkout at {} is no longer on disk",
                checkout.display()
            ));
        }
        Err(error) => {
            return ForgetVerification::Withheld(format!(
                "the checkout at {} could not be read: {error}",
                checkout.display()
            ));
        }
    }
    match checkout.canonicalize() {
        Ok(resolved) if resolved == checkout => {}
        Ok(resolved) => {
            return ForgetVerification::Failed(format!(
                "the checkout at {} now resolves to {}",
                checkout.display(),
                resolved.display()
            ));
        }
        Err(error) => {
            return ForgetVerification::Withheld(format!(
                "the checkout at {} could not be resolved: {error}",
                checkout.display()
            ));
        }
    }
    // A repository's own `.git` is a filesystem fact, and asking for it first
    // keeps the definite answer definite: an empty directory standing where the
    // checkout stood is not something Git can be asked about at all. Absent is
    // that answer; a lookup the filesystem refused is not an answer.
    if let Err(error) = fs::symlink_metadata(checkout.join(".git")) {
        return if error.kind() == io::ErrorKind::NotFound {
            ForgetVerification::Failed(format!(
                "the directory at {} is no longer a Git checkout",
                checkout.display()
            ))
        } else {
            ForgetVerification::Withheld(format!(
                "the checkout at {} could not be read: {error}",
                checkout.display()
            ))
        };
    }
    match look_up_revision(checkout, plan.source.head()) {
        Ok(RevisionLookup::Present) => ForgetVerification::Held,
        Ok(RevisionLookup::Absent) => ForgetVerification::Failed(format!(
            "the checkout at {} no longer contains the revision it was registered at",
            checkout.display()
        )),
        Ok(RevisionLookup::Undetermined(message)) => ForgetVerification::Withheld(format!(
            "the checkout at {} could not be verified: {message}",
            checkout.display()
        )),
        Err(error) => ForgetVerification::Withheld(format!(
            "the checkout at {} could not be verified: {error}",
            checkout.display()
        )),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ForgetPrompt {
    Preview(ForgetPlan),
    Report(ForgetOutcome),
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
    let mut held = Vec::new();
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
                postcondition: Postcondition::LinkAsPlanned,
                reason,
                required: true,
                precluded_by_selection: false,
            });
            continue;
        }
        let Some(observed) = row.and_then(|row| row.observation(step.agent)) else {
            failures.push(VerifyFailure {
                agent: step.agent,
                postcondition: Postcondition::LinkAsPlanned,
                observed: "the scan taken afterwards found nothing at this path".to_owned(),
            });
            continue;
        };
        match mismatch(plan, observed) {
            Checked::Held => held.push(VerifyPass {
                agent: step.agent,
                postcondition: Postcondition::LinkAsPlanned,
            }),
            Checked::Failed(observed) => {
                failures.push(VerifyFailure {
                    agent: step.agent,
                    postcondition: Postcondition::LinkAsPlanned,
                    observed,
                });
                continue;
            }
            Checked::Withheld(reason) => {
                withheld.push(VerifyWithheld {
                    agent: step.agent,
                    postcondition: Postcondition::LinkAsPlanned,
                    reason,
                    required: true,
                    precluded_by_selection: false,
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
            held.push(VerifyPass {
                agent: step.agent,
                postcondition: Postcondition::OpenCodeResolution,
            });
            continue;
        }
        if let OpenCodeResolution::Incomplete { roots } = resolution {
            withheld.push(VerifyWithheld {
                agent: step.agent,
                postcondition: Postcondition::OpenCodeResolution,
                reason: format!(
                    "what OpenCode resolves the name to could not be established: {}",
                    unknown_roots(roots)
                ),
                required: false,
                precluded_by_selection: unknown_only_by_selection(roots, snapshot),
            });
            continue;
        }
        failures.push(VerifyFailure {
            agent: step.agent,
            postcondition: Postcondition::OpenCodeResolution,
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
                    postcondition: Postcondition::OpenCodeResolution,
                    reason: format!(
                        "what OpenCode resolves the name to could not be established: {}",
                        unknown_roots(roots)
                    ),
                    required: false,
                    precluded_by_selection: unknown_only_by_selection(roots, snapshot),
                });
            }
            (Some(expected), actual) if expected != &actual => failures.push(VerifyFailure {
                agent: AgentKind::OpenCode,
                postcondition: Postcondition::OpenCodeResolution,
                observed: format!(
                    "this was not what the plan described: {}",
                    observed_summary(observed)
                ),
            }),
            (Some(_), _) => held.push(VerifyPass {
                agent: AgentKind::OpenCode,
                postcondition: Postcondition::OpenCodeResolution,
            }),
            (None, _) => {}
        }
    }
    VerifyReport {
        held,
        failures,
        withheld,
    }
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

/// Whether the user's own agent selection is the only reason these roots went
/// unread: every one of them was skipped because it was deselected, and none
/// was unreadable, unscanned for another reason, or read but unresolvable.
fn unknown_only_by_selection(roots: &[UnknownRoot], snapshot: &InventorySnapshot) -> bool {
    roots.iter().all(|root| {
        root.cause() == UnknownCause::RootNotRead
            && matches!(snapshot.root(root.root()).status(), RootStatus::NotSelected)
    })
}

/// Whether every gap in what this scan established under one name traces only
/// to the user's own agent selection. A deselected agent must not mask a
/// *selected* root the scan could not read — nor a selected root it did read
/// whose entry under the name could not be followed: the check that consults
/// this was kept from running by both, and only deselection is the user's own
/// choice.
fn scan_gaps_only_by_selection(row: Option<&InventoryRow>, snapshot: &InventorySnapshot) -> bool {
    AgentKind::ALL.into_iter().all(|agent| {
        match snapshot.root(agent).status() {
            RootStatus::NotSelected => return true,
            status if unscanned(status).is_some() => return false,
            _ => {}
        }
        row.and_then(|row| row.observation(agent))
            .is_none_or(|observation| !matches!(observation.sighting(), RootSighting::Unknown))
    })
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
    use crate::store::Store;

    /// Only the Unix install fixtures build a checkout, so the helper is theirs.
    #[cfg(unix)]
    fn git(repository: &Path, arguments: &[&str]) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .output()
            .expect("run git fixture command");
        assert!(output.status.success(), "git {arguments:?} failed");
    }

    /// One registered checkout holding one portable skill, and a store that
    /// knows about it. Everything lives under a canonical temporary directory
    /// so no test reads the real home or a real agent skill root.
    #[cfg(unix)]
    fn installable_fixture() -> (tempfile::TempDir, PathBuf, Store, InstallPlan) {
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let base = fixture
            .path()
            .canonicalize()
            .expect("canonical fixture path");
        let checkout = base.join("library");
        let variant_directory = checkout.join("skills/portable");
        fs::create_dir_all(&variant_directory).expect("variant directory");
        fs::write(
            variant_directory.join("SKILL.md"),
            "---\nname: portable\ndescription: portable fixture\n---\n",
        )
        .expect("write skill document");
        git(&checkout, &["init", "--quiet"]);
        git(&checkout, &["config", "user.name", "Test Author"]);
        git(&checkout, &["config", "user.email", "test@example.com"]);
        git(&checkout, &["add", "."]);
        git(&checkout, &["commit", "--quiet", "-m", "fixture"]);

        let home = base.join("home");
        let root = home.join(".claude/skills");
        fs::create_dir_all(&root).expect("agent skill root");

        let mut store = Store::open(&base.join("data")).expect("metadata store");
        let preview = crate::source::preview_local_source(&checkout).expect("preview the checkout");
        store
            .register_source(&preview)
            .expect("register the source");
        let sources = store.registered_sources().expect("registered sources");
        let source = sources.first().expect("one registered source");
        let variant = variants_by_name(&sources)
            .remove("portable")
            .and_then(|mut variants| variants.pop())
            .expect("the registered variant");

        let plan = InstallPlan {
            variant,
            registry: RegistryFingerprint::of_registry(&sources),
            source_checkout: source.git_top_level().to_path_buf(),
            source_revision: source.head().to_owned(),
            source_dir: variant_directory,
            targets: vec![InstallTarget {
                agent: AgentKind::ClaudeCode,
                link_path: root.join("portable"),
                disposition: TargetDisposition::CreateLink,
            }],
            warnings: Vec::new(),
            opencode_outlook: None,
        };
        (fixture, home, store, plan)
    }

    /// The same fixture with nothing happening in between writes the link the
    /// plan described, so the refusal below is the concurrent deletion rather
    /// than a fixture that could never install anything.
    #[cfg(unix)]
    #[test]
    fn an_undisturbed_install_creates_the_link_the_plan_described() {
        let (_fixture, home, mut store, plan) = installable_fixture();

        let applied = apply_install(&plan, &mut store, &home);

        let outcome = &applied.steps[0].outcome;
        assert!(
            matches!(outcome, StepOutcome::Created),
            "the undisturbed install must create its link: {outcome:?}"
        );
        assert_eq!(
            fs::read_link(plan.targets()[0].link_path()).expect("the created link"),
            plan.source_dir()
        );
    }

    /// A held preview is evidence about a moment that has passed. Another
    /// Skilled process fast-forwarding the same checkout can delete the variant
    /// directory while this install is still in the metadata work between its
    /// first read of that directory and the `symlink` call — the store guard
    /// waits on another process's transaction, and the identity recheck spawns
    /// Git. A link created over that gap would be dangling, which is not what
    /// the preview described, so the link target is read once more after every
    /// wait and this target is refused instead.
    #[cfg(unix)]
    #[test]
    fn a_variant_directory_removed_during_the_metadata_work_writes_no_link() {
        let (_fixture, home, mut store, plan) = installable_fixture();
        let variant_directory = plan.source_dir().to_path_buf();
        let link_path = plan.targets()[0].link_path().to_path_buf();
        // Stands in for the concurrent process: it runs after the plan's first
        // read of the link target and before anything is written.
        set_concurrent_target_change(move || {
            fs::remove_dir_all(&variant_directory).expect("the concurrent update removes it");
        });

        let applied = apply_install(&plan, &mut store, &home);

        let outcome = &applied.steps[0].outcome;
        assert!(
            matches!(outcome, StepOutcome::Failed(reason)
                if reason.contains("no longer the directory the plan resolved")),
            "the vanished link target must stop the write: {outcome:?}"
        );
        assert!(
            fs::symlink_metadata(&link_path).is_err(),
            "no link may be created over a variant directory that is gone"
        );
    }

    /// The Windows repair deletes only an object it has proven through the
    /// same handle, and the proof reads the reparse data itself. The parser
    /// and the normalisation the comparison rests on are pure so their
    /// answers can be pinned on every platform.
    mod reparse {
        use super::super::{
            IO_REPARSE_TAG_SYMLINK, SYMLINK_FLAG_RELATIVE, comparable_symlink_target,
            symlink_reparse_target,
        };

        fn reparse_buffer(tag: u32, flags: u32, substitute: &[u16]) -> Vec<u8> {
            let mut bytes = Vec::new();
            let name: Vec<u8> = substitute
                .iter()
                .flat_map(|unit| unit.to_le_bytes())
                .collect();
            let data_length = (12 + name.len()) as u16;
            bytes.extend_from_slice(&tag.to_le_bytes());
            bytes.extend_from_slice(&data_length.to_le_bytes());
            bytes.extend_from_slice(&0_u16.to_le_bytes()); // Reserved
            bytes.extend_from_slice(&0_u16.to_le_bytes()); // SubstituteNameOffset
            bytes.extend_from_slice(&(name.len() as u16).to_le_bytes());
            bytes.extend_from_slice(&(name.len() as u16).to_le_bytes()); // PrintNameOffset
            bytes.extend_from_slice(&0_u16.to_le_bytes()); // PrintNameLength
            bytes.extend_from_slice(&flags.to_le_bytes());
            bytes.extend_from_slice(&name);
            bytes
        }

        fn wide(text: &str) -> Vec<u16> {
            text.encode_utf16().collect()
        }

        #[test]
        fn the_substitute_name_and_relative_flag_are_reported_raw() {
            let buffer = reparse_buffer(IO_REPARSE_TAG_SYMLINK, 0, &wide("\\??\\C:\\skills\\demo"));
            assert_eq!(
                symlink_reparse_target(&buffer),
                Ok((wide("\\??\\C:\\skills\\demo"), false))
            );
            let buffer = reparse_buffer(
                IO_REPARSE_TAG_SYMLINK,
                SYMLINK_FLAG_RELATIVE,
                &wide("..\\demo"),
            );
            assert_eq!(
                symlink_reparse_target(&buffer),
                Ok((wide("..\\demo"), true))
            );
        }

        #[test]
        fn a_mount_point_is_not_a_symbolic_link() {
            // IO_REPARSE_TAG_MOUNT_POINT: a junction is a different object,
            // and Skilled never installed one.
            let buffer = reparse_buffer(0xA000_0003, 0, &wide("\\??\\C:\\elsewhere"));
            assert!(symlink_reparse_target(&buffer).is_err());
        }

        #[test]
        fn a_length_beyond_the_buffer_is_refused() {
            let mut buffer = reparse_buffer(IO_REPARSE_TAG_SYMLINK, 0, &wide("\\??\\C:\\x"));
            buffer[4] = 0xFF;
            buffer[5] = 0x7F;
            assert!(symlink_reparse_target(&buffer).is_err());
        }

        #[test]
        fn a_target_region_outside_the_stated_data_is_refused() {
            let mut buffer = reparse_buffer(IO_REPARSE_TAG_SYMLINK, 0, &wide("\\??\\C:\\x"));
            // SubstituteNameOffset pushed past the stated data length.
            buffer[8] = 0xF0;
            assert!(symlink_reparse_target(&buffer).is_err());
        }

        /// One comparable spelling for the raw substitute name and every form
        /// `std::fs::read_link` reports for the same target: the NT and
        /// verbatim prefixes come off, a namespaced `UNC\` becomes the
        /// familiar `\\`, and everything else is untouched.
        #[test]
        fn nt_verbatim_and_user_spellings_of_one_target_compare_equal() {
            for (stored, reported) in [
                ("\\??\\C:\\skills\\demo", "C:\\skills\\demo"),
                (
                    "\\??\\UNC\\server\\share\\skill",
                    "\\\\server\\share\\skill",
                ),
                // A path read_link keeps verbatim still names the same object.
                ("\\??\\C:\\needs verbatim.", "\\\\?\\C:\\needs verbatim."),
                ("..\\demo", "..\\demo"),
            ] {
                assert_eq!(
                    comparable_symlink_target(&wide(stored)),
                    comparable_symlink_target(&wide(reported)),
                    "{stored} should compare equal to {reported}"
                );
            }
        }

        #[test]
        fn distinct_targets_stay_distinct_after_normalisation() {
            for (one, other) in [
                ("\\??\\C:\\skills\\demo", "C:\\skills\\demo2"),
                // A relative directory literally named UNC is not a share.
                ("UNC\\server\\share", "\\\\server\\share"),
                ("\\??\\C:\\skills\\demo.", "C:\\skills\\demo"),
            ] {
                assert_ne!(
                    comparable_symlink_target(&wide(one)),
                    comparable_symlink_target(&wide(other)),
                    "{one} must not compare equal to {other}"
                );
            }
        }
    }

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

    #[cfg(unix)]
    #[test]
    fn receipt_finalization_rechecks_that_the_managed_link_is_still_gone() {
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let target = fixture.path().join("target");
        let link = fixture.path().join("home/.claude/skills/portable");
        fs::create_dir_all(&target).expect("target directory");
        fs::create_dir_all(link.parent().expect("link root")).expect("link root");
        std::os::unix::fs::symlink(&target, &link).expect("recreated managed link");
        let mut store = Store::open(&fixture.path().join("data")).expect("metadata store");
        let receipt = Receipt::new(
            ReceiptOperation::Install,
            AgentKind::ClaudeCode,
            "portable".to_owned(),
            link.clone(),
            target.clone(),
            None,
            None,
            None,
        );
        record_test_receipt(&mut store, &receipt);
        let (plan, applied) = removable_uninstall(&link, &target, true);
        let verification = VerifyReport {
            held: vec![
                VerifyPass {
                    agent: AgentKind::ClaudeCode,
                    postcondition: Postcondition::LinkGone,
                },
                VerifyPass {
                    agent: AgentKind::ClaudeCode,
                    postcondition: Postcondition::ContentSurvived,
                },
            ],
            ..VerifyReport::default()
        };

        let finalized = finalize_uninstall(&plan, &applied, &verification, &mut store);

        assert_eq!(finalized.failures().len(), 1);
        assert_eq!(
            uninstall_status(&applied, &verification, &finalized),
            UninstallStatus::VerificationFailed
        );
        assert_eq!(store.receipts().expect("retained receipt"), vec![receipt]);
    }

    #[test]
    fn an_unreadable_receipted_target_remains_removable_with_its_state_preserved() {
        let link = PathBuf::from("/home/example/.claude/skills/portable");
        let target = PathBuf::from("/source/skills/portable");
        let slot = TargetProbe {
            agent: AgentKind::ClaudeCode,
            link_path: link.clone(),
            root: RootProbe::Present,
            entry: EntryProbe::Symlink {
                target: target.clone(),
                canonical: None,
                target_state: UninstallTargetState::Unreadable("permission denied".to_owned()),
            },
            content: SlotContent::Unknown,
        };
        let receipt = Receipt::new(
            ReceiptOperation::Install,
            AgentKind::ClaudeCode,
            "portable".to_owned(),
            link,
            target,
            None,
            None,
            None,
        );

        let disposition = uninstall_disposition(&slot, &[&receipt]);

        let UninstallDisposition::RemoveLink { target_state, .. } = disposition else {
            panic!("an unreadable exact receipted target remains removable")
        };
        assert_eq!(
            target_state,
            UninstallTargetState::Unreadable("permission denied".to_owned())
        );
    }

    #[test]
    fn a_receipted_target_replaced_by_a_file_remains_removable_with_its_state_preserved() {
        let link = PathBuf::from("/home/example/.claude/skills/portable");
        let target = PathBuf::from("/source/skills/portable");
        let slot = TargetProbe {
            agent: AgentKind::ClaudeCode,
            link_path: link.clone(),
            root: RootProbe::Present,
            entry: EntryProbe::Symlink {
                target: target.clone(),
                canonical: Some(target.clone()),
                target_state: UninstallTargetState::NotADirectory,
            },
            content: SlotContent::Nowhere,
        };
        let receipt = Receipt::new(
            ReceiptOperation::Install,
            AgentKind::ClaudeCode,
            "portable".to_owned(),
            link,
            target,
            None,
            None,
            None,
        );

        let disposition = uninstall_disposition(&slot, &[&receipt]);

        let UninstallDisposition::RemoveLink { target_state, .. } = disposition else {
            panic!("a non-directory exact receipted target remains removable")
        };
        assert_eq!(target_state, UninstallTargetState::NotADirectory);
    }

    #[test]
    fn receipt_finalization_depends_only_on_the_positive_link_gone_pass() {
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let target = fixture.path().join("target");
        let link = fixture.path().join("home/.claude/skills/portable");
        fs::create_dir_all(&target).expect("target directory");
        let mut store = Store::open(&fixture.path().join("data")).expect("metadata store");
        let receipt = Receipt::new(
            ReceiptOperation::Install,
            AgentKind::ClaudeCode,
            "portable".to_owned(),
            link.clone(),
            target.clone(),
            None,
            None,
            None,
        );
        record_test_receipt(&mut store, &receipt);
        let (plan, applied) = removable_uninstall(&link, &target, true);
        let verification = VerifyReport {
            held: vec![VerifyPass {
                agent: AgentKind::ClaudeCode,
                postcondition: Postcondition::LinkGone,
            }],
            ..VerifyReport::default()
        };

        let finalized = finalize_uninstall(&plan, &applied, &verification, &mut store);

        assert!(finalized.failures().is_empty());
        assert!(store.receipts().expect("receipt removed").is_empty());
    }

    #[test]
    fn receipt_finalization_reports_when_link_gone_was_not_established() {
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let target = fixture.path().join("target");
        let link = fixture.path().join("home/.claude/skills/portable");
        fs::create_dir_all(&target).expect("target directory");
        let mut store = Store::open(&fixture.path().join("data")).expect("metadata store");
        let receipt = Receipt::new(
            ReceiptOperation::Install,
            AgentKind::ClaudeCode,
            "portable".to_owned(),
            link.clone(),
            target.clone(),
            None,
            None,
            None,
        );
        record_test_receipt(&mut store, &receipt);
        let (plan, applied) = removable_uninstall(&link, &target, true);

        let finalized = finalize_uninstall(&plan, &applied, &VerifyReport::default(), &mut store);

        assert_eq!(finalized.failures().len(), 1);
        assert!(
            finalized.failures()[0]
                .reason()
                .contains("not positively verified")
        );
        assert_eq!(store.receipts().expect("retained receipt"), vec![receipt]);
    }

    #[test]
    fn receipt_finalization_does_not_make_ancillary_content_control_cleanup() {
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let target = fixture.path().join("target");
        let link = fixture.path().join("home/.claude/skills/portable");
        fs::create_dir_all(&target).expect("target directory");
        let mut store = Store::open(&fixture.path().join("data")).expect("metadata store");
        let receipt = Receipt::new(
            ReceiptOperation::Install,
            AgentKind::ClaudeCode,
            "portable".to_owned(),
            link.clone(),
            target.clone(),
            None,
            None,
            None,
        );
        record_test_receipt(&mut store, &receipt);
        let (plan, applied) = removable_uninstall(&link, &target, true);
        let verification = VerifyReport {
            held: vec![
                VerifyPass {
                    agent: AgentKind::ClaudeCode,
                    postcondition: Postcondition::LinkGone,
                },
                VerifyPass {
                    agent: AgentKind::ClaudeCode,
                    postcondition: Postcondition::ContentSurvived,
                },
            ],
            ..VerifyReport::default()
        };
        fs::remove_dir(&target).expect("content disappears after verification");

        let finalized = finalize_uninstall(&plan, &applied, &verification, &mut store);

        assert!(finalized.failures().is_empty());
        assert!(store.receipts().expect("receipt removed").is_empty());
    }

    fn removable_uninstall(
        link_path: &Path,
        link_target: &Path,
        resolves: bool,
    ) -> (UninstallPlan, ApplyReport) {
        (
            UninstallPlan {
                skill_name: "portable".to_owned(),
                targets: vec![UninstallTarget {
                    agent: AgentKind::ClaudeCode,
                    link_path: link_path.to_path_buf(),
                    disposition: UninstallDisposition::RemoveLink {
                        link_target: link_target.to_path_buf(),
                        target_state: if resolves {
                            UninstallTargetState::Directory
                        } else {
                            UninstallTargetState::Missing
                        },
                        receipts: Vec::new(),
                    },
                }],
                warnings: Vec::new(),
                opencode_outlook: None,
            },
            ApplyReport {
                steps: vec![AppliedStep {
                    agent: AgentKind::ClaudeCode,
                    link_path: link_path.to_path_buf(),
                    outcome: StepOutcome::Removed,
                }],
            },
        )
    }

    fn record_test_receipt(store: &mut Store, receipt: &Receipt) {
        let mutation = store.begin_mutation().expect("receipt mutation guard");
        mutation.record_receipt(receipt).expect("record receipt");
        mutation.commit().expect("commit receipt");
    }

    /// Windows must remove a directory symlink before it can create its
    /// replacement. If creation then fails, that is a partial mutation rather
    /// than the same `NotApplied` state as a guard refusal.
    #[test]
    fn an_old_link_removed_before_replacement_failure_is_a_partial_repair() {
        let mut plan = empty_repair_plan(
            AgentKind::Codex,
            RegistryFingerprint::of_registry(&[]),
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
            RegistryFingerprint::of_registry(&[]),
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

    /// A replacement written into a root that moved during the repair is a
    /// partial mutation of its own kind: the link is live but unreceipted at
    /// a path the plan never stated, which must never render as disposable
    /// temporary residue.
    #[test]
    fn a_replacement_in_a_moved_root_is_a_partial_repair() {
        let mut plan = empty_repair_plan(
            AgentKind::Codex,
            RegistryFingerprint::of_registry(&[]),
            "portable",
            PathBuf::from("/home/example/.agents/skills/portable"),
        );
        plan.disposition = RepairDisposition::ReplaceLink { dangling: false };
        let applied = RepairApplyReport {
            step: Some(RepairAppliedStep {
                agent: AgentKind::Codex,
                link_path: plan.link_path.clone(),
                outcome: RepairStepOutcome::MovedRootUnreceipted {
                    path: PathBuf::from("/home/example/renamed-skills/portable"),
                    error: "the skill root was renamed while the repair ran".to_owned(),
                },
            }),
        };
        let outcome = RepairOutcome::new(plan, applied, VerifyReport::default());

        assert_eq!(outcome.status(), RepairStatus::PartiallyApplied);
    }

    /// A displaced object stranded beside a completed replacement is a
    /// residual write outstanding, so the repair reports as partial even
    /// though its link and receipt are complete.
    #[test]
    fn a_stranded_displaced_object_is_a_partial_repair() {
        let mut plan = empty_repair_plan(
            AgentKind::Codex,
            RegistryFingerprint::of_registry(&[]),
            "portable",
            PathBuf::from("/home/example/.agents/skills/portable"),
        );
        plan.disposition = RepairDisposition::ReplaceLink { dangling: false };
        let applied = RepairApplyReport {
            step: Some(RepairAppliedStep {
                agent: AgentKind::Codex,
                link_path: plan.link_path.clone(),
                outcome: RepairStepOutcome::RepairedResidualTemporary {
                    path: PathBuf::from("/home/example/.agents/skills/.skilled-repair-123-456"),
                    error: "permission denied".to_owned(),
                },
            }),
        };
        let outcome = RepairOutcome::new(plan, applied, VerifyReport::default());

        assert_eq!(outcome.status(), RepairStatus::PartiallyApplied);
    }

    /// Cleaning up a temporary entry must re-prove what it removes, relative
    /// to the pinned parent directory: a link whose raw target matches is
    /// removed, anything else is preserved.
    #[cfg(unix)]
    #[test]
    fn temporary_cleanup_removes_only_a_proven_link() {
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let proven = fixture.path().join("proven-target");
        let matching = fixture.path().join("matching");
        std::os::unix::fs::symlink(&proven, &matching).expect("matching link");
        let differing = fixture.path().join("differing");
        std::os::unix::fs::symlink(fixture.path().join("elsewhere"), &differing)
            .expect("differing link");
        let occupied = fixture.path().join("occupied");
        fs::write(&occupied, b"arrived data").expect("arrived file");
        let dir = open_pinned_parent_via(fixture.path(), fixture.path())
            .expect("pinned parent directory");
        let name = |name: &str| entry_name_arg(std::ffi::OsStr::new(name)).expect("entry name");

        assert!(matches!(
            remove_proven_temporary(&dir, &name("matching"), &proven),
            TemporaryCleanup::Removed
        ));
        assert!(fs::symlink_metadata(&matching).is_err());

        assert!(matches!(
            remove_proven_temporary(&dir, &name("differing"), &proven),
            TemporaryCleanup::NotProven { .. }
        ));
        assert!(fs::symlink_metadata(&differing).is_ok());

        assert!(matches!(
            remove_proven_temporary(&dir, &name("occupied"), &proven),
            TemporaryCleanup::NotProven { .. }
        ));
        assert_eq!(
            fs::read(&occupied).expect("preserved file"),
            b"arrived data"
        );
    }

    /// A residual report must name where the residue actually is: the pinned
    /// descriptor resolves its current pathname even after the directory was
    /// renamed, so a report never points at a path the object no longer has.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn the_pinned_directory_reports_its_current_path_after_a_rename() {
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let original = fixture.path().join("root-a");
        fs::create_dir_all(&original).expect("original root");
        let dir = open_pinned_parent_via(fixture.path(), &original).expect("pinned directory");
        let renamed = fixture.path().join("root-b");
        fs::rename(&original, &renamed).expect("root renamed");

        let reported = pinned_directory_path(&dir).expect("current pathname");

        assert_eq!(
            reported,
            fs::canonicalize(&renamed).expect("renamed root resolves")
        );
    }

    /// Success may claim the planned pathname only while that pathname still
    /// names the pinned directory; a rename must be detected so no receipt is
    /// recorded against a path the replacement no longer has.
    #[cfg(unix)]
    #[test]
    fn the_pinned_directory_knows_when_its_pathname_moved() {
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let original = fixture.path().join("root-a");
        fs::create_dir_all(&original).expect("original root");
        let dir = open_pinned_parent_via(fixture.path(), &original).expect("pinned directory");

        assert!(pinned_directory_still_via(&dir, fixture.path(), &original));

        let renamed = fixture.path().join("root-b");
        fs::rename(&original, &renamed).expect("root renamed");

        assert!(!pinned_directory_still_via(&dir, fixture.path(), &original));

        fs::create_dir_all(&original).expect("impostor at the original path");

        assert!(!pinned_directory_still_via(&dir, fixture.path(), &original));

        fs::remove_dir(&original).expect("impostor removed");
        std::os::unix::fs::symlink(&renamed, &original).expect("alias at the original path");

        assert!(
            !pinned_directory_still_via(&dir, fixture.path(), &original),
            "a symlink alias to the pinned directory must not pass as the root itself"
        );
    }

    /// `O_NOFOLLOW` guards only the final component, so the walk from the
    /// validated base must refuse a symbolic link at every step: an ancestor
    /// swapped for a link after the guards must not redirect the pin, even
    /// when the redirected tree holds a byte-identical proven link.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn replace_refuses_a_symlinked_ancestor_between_base_and_root() {
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let real = fixture.path().join("real");
        let root_in_real = real.join("skills");
        fs::create_dir_all(&root_in_real).expect("redirected skill root");
        let old_target = fixture.path().join("old-target");
        let new_target = fixture.path().join("new-target");
        fs::create_dir_all(&old_target).expect("old target directory");
        fs::create_dir_all(&new_target).expect("new target directory");
        std::os::unix::fs::symlink(&old_target, root_in_real.join("portable"))
            .expect("byte-identical link in the redirected tree");
        let alias = fixture.path().join("agent");
        std::os::unix::fs::symlink(&real, &alias).expect("swapped ancestor");
        let destination = alias.join("skills").join("portable");

        let error =
            replace_directory_symlink(&new_target, &destination, &old_target, fixture.path())
                .expect_err("a symlinked ancestor refuses the replacement");

        assert!(
            matches!(error, ReplaceLinkError::Unchanged(_)),
            "unexpected error: {error:?}"
        );
        assert_eq!(
            fs::read_link(root_in_real.join("portable")).expect("untouched link"),
            old_target
        );
        assert_eq!(
            fs::read_dir(&root_in_real)
                .expect("redirected root listing")
                .count(),
            1,
            "the redirected tree must gain nothing"
        );
    }

    /// The pinned parent refuses to open through a symbolic link, re-enforcing
    /// the root-is-not-a-link invariant at the descriptor that every write
    /// then goes through.
    #[cfg(unix)]
    #[test]
    fn the_pinned_parent_refuses_a_symlinked_directory() {
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let real = fixture.path().join("real-root");
        fs::create_dir_all(&real).expect("real root");
        let alias = fixture.path().join("alias-root");
        std::os::unix::fs::symlink(&real, &alias).expect("root alias");

        assert!(open_pinned_parent_via(fixture.path(), &alias).is_err());
        assert!(open_pinned_parent_via(fixture.path(), &real).is_ok());
        assert!(
            open_pinned_parent_via(&alias, &alias).is_ok(),
            "the trust anchor itself may be reached through a link"
        );
    }

    /// The replacement must displace exactly the proven link: the exchange
    /// swaps it out whole, the displaced link is verified against the recorded
    /// raw target, and nothing else is left in the skill root afterwards.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn replace_exchanges_the_proven_link_for_the_replacement() {
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let root = fixture.path().join("skills");
        fs::create_dir_all(&root).expect("skill root");
        let old_target = fixture.path().join("old-target");
        let new_target = fixture.path().join("new-target");
        fs::create_dir_all(&old_target).expect("old target directory");
        fs::create_dir_all(&new_target).expect("new target directory");
        let link = root.join("portable");
        std::os::unix::fs::symlink(&old_target, &link).expect("proven link");

        let replaced = replace_directory_symlink(&new_target, &link, &old_target, fixture.path())
            .expect("replacing the proven link succeeds");

        assert!(
            replaced.residue.is_none(),
            "a clean replacement leaves no residue"
        );
        assert_eq!(fs::read_link(&link).expect("replaced link"), new_target);
        let entries: Vec<_> = fs::read_dir(&root)
            .expect("skill root listing")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("portable")]);
    }

    /// A regular file that arrives at the destination after the final recheck
    /// is exactly what `rename(2)` would have destroyed. The replacement must
    /// refuse, and the file must survive at the destination with its content,
    /// with no temporary residue beside it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn replace_preserves_a_file_that_arrived_at_the_destination() {
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let root = fixture.path().join("skills");
        fs::create_dir_all(&root).expect("skill root");
        let proven_target = fixture.path().join("old-target");
        let new_target = fixture.path().join("new-target");
        fs::create_dir_all(&new_target).expect("new target directory");
        let destination = root.join("portable");
        fs::write(&destination, b"a stranger's data").expect("arrived file");

        let error =
            replace_directory_symlink(&new_target, &destination, &proven_target, fixture.path())
                .expect_err("an unproven occupant refuses the replacement");

        assert!(
            matches!(error, ReplaceLinkError::ConcurrentlyReplaced { .. }),
            "unexpected error: {error:?}"
        );
        assert_eq!(
            fs::read(&destination).expect("preserved file"),
            b"a stranger's data"
        );
        let entries: Vec<_> = fs::read_dir(&root)
            .expect("skill root listing")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("portable")]);
    }

    /// A symbolic link whose raw target is not byte-identical to the proven
    /// one is a stranger's link, not the link repair proved. It must be
    /// restored unread and unfollowed, with its raw target intact.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn replace_restores_a_stranger_link_with_its_raw_target_intact() {
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let root = fixture.path().join("skills");
        fs::create_dir_all(&root).expect("skill root");
        let proven_target = fixture.path().join("old-target");
        let new_target = fixture.path().join("new-target");
        fs::create_dir_all(&new_target).expect("new target directory");
        let destination = root.join("portable");
        let stranger_target = PathBuf::from("../somewhere/else");
        std::os::unix::fs::symlink(&stranger_target, &destination).expect("arrived link");

        let error =
            replace_directory_symlink(&new_target, &destination, &proven_target, fixture.path())
                .expect_err("an unproven link refuses the replacement");

        assert!(
            matches!(error, ReplaceLinkError::ConcurrentlyReplaced { .. }),
            "unexpected error: {error:?}"
        );
        assert_eq!(
            fs::read_link(&destination).expect("preserved link"),
            stranger_target
        );
        let entries: Vec<_> = fs::read_dir(&root)
            .expect("skill root listing")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("portable")]);
    }

    /// Repair never recreates an absent link. A destination that vanished
    /// after the final recheck refuses the replacement and stays absent — the
    /// skill root gains nothing, not even transiently visible residue.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn replace_refuses_an_absent_destination_and_creates_nothing() {
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let root = fixture.path().join("skills");
        fs::create_dir_all(&root).expect("skill root");
        let proven_target = fixture.path().join("old-target");
        let new_target = fixture.path().join("new-target");
        fs::create_dir_all(&new_target).expect("new target directory");
        let destination = root.join("portable");

        let error =
            replace_directory_symlink(&new_target, &destination, &proven_target, fixture.path())
                .expect_err("an absent destination refuses the replacement");

        assert!(
            matches!(error, ReplaceLinkError::Unchanged(_)),
            "unexpected error: {error:?}"
        );
        assert!(fs::symlink_metadata(&destination).is_err());
        assert_eq!(
            fs::read_dir(&root).expect("skill root listing").count(),
            0,
            "the skill root must be left empty"
        );
    }

    /// A Unix platform without an atomic exchange refuses the repair rather
    /// than falling back to a destructive rename: the proven link survives
    /// untouched and the skill root gains no residue.
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
    #[test]
    fn replace_refuses_where_no_atomic_exchange_exists() {
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let root = fixture.path().join("skills");
        fs::create_dir_all(&root).expect("skill root");
        let old_target = fixture.path().join("old-target");
        let new_target = fixture.path().join("new-target");
        fs::create_dir_all(&old_target).expect("old target directory");
        fs::create_dir_all(&new_target).expect("new target directory");
        let destination = root.join("portable");
        std::os::unix::fs::symlink(&old_target, &destination).expect("proven link");

        let error =
            replace_directory_symlink(&new_target, &destination, &old_target, fixture.path())
                .expect_err("a platform without exchange refuses the replacement");

        assert!(
            matches!(error, ReplaceLinkError::ExchangeUnsupported(_)),
            "unexpected error: {error:?}"
        );
        assert_eq!(
            fs::read_link(&destination).expect("surviving link"),
            old_target
        );
        let entries: Vec<_> = fs::read_dir(&root)
            .expect("skill root listing")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("portable")]);
    }
}
