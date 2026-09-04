use std::{
    path::{Path, PathBuf},
    process::Child,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, AtomicU8, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread::JoinHandle,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    AgentDetection, AgentKind, AppEnvironment, Error, MetadataFailure, Result, SessionIdentity,
    agents::{detect_agents, detection_at},
    inventory::{
        DoctorEntry, Finding, FindingSeverity, InstallationObject, InstalledSkillObservation,
        InventoryRow, InventorySnapshot, RegistryAvailability, doctor_order, scan_installations,
    },
    operations::{
        ForgetOutcome, ForgetPlan, ForgetPrompt, InstallOutcome, InstallPlan, InstallPrompt,
        OperationPrompt, Receipt, RepairOfferStatus, RepairOutcome, RepairOverlay, RepairPlan,
        RepairPrompt, UninstallOutcome, UninstallPlan, UninstallPrompt, apply_forget,
        apply_install, apply_repair, apply_uninstall, finalize_uninstall, plan_forget,
        plan_forget_unreadable_receipts, plan_install, plan_repair, plan_uninstall, probe_forget,
        probe_install, probe_repair, probe_uninstall, probe_uninstall_content, verify_forget,
        verify_install, verify_repair, verify_uninstall,
    },
    resolution::VariantRef,
    source::{
        CatalogProposal, RegisteredSource, SkillCandidate, SourcePreview, preview_local_source,
        revalidate_source_preview,
    },
    store::Store,
    updates::{
        CachedUpdateCheck, RepositoryUpdatePlan, RepositoryUpdatePrompt, RepositoryUpdateVerdict,
        apply_repository_update_attempt, cached_update_check, encode_findings,
        plan_repository_update, probe_repository_update, probe_repository_update_against,
        probe_repository_update_cancellable, verify_repository_update_attempt,
    },
    validation::valid_skill_name,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetupStep {
    Welcome,
    DetectAgents,
    ChooseScanRoots,
    DiscoverSources,
    ConfirmCatalogs,
    ScanInstallations,
    Summary,
}

impl SetupStep {
    fn next(self) -> Option<Self> {
        match self {
            Self::Welcome => Some(Self::DetectAgents),
            Self::DetectAgents => Some(Self::ChooseScanRoots),
            Self::ChooseScanRoots => Some(Self::DiscoverSources),
            Self::DiscoverSources => Some(Self::ConfirmCatalogs),
            Self::ConfirmCatalogs => Some(Self::ScanInstallations),
            Self::ScanInstallations => Some(Self::Summary),
            Self::Summary => None,
        }
    }

    fn previous(self) -> Option<Self> {
        match self {
            Self::Welcome => None,
            Self::DetectAgents => Some(Self::Welcome),
            Self::ChooseScanRoots => Some(Self::DetectAgents),
            Self::DiscoverSources => Some(Self::ChooseScanRoots),
            Self::ConfirmCatalogs => Some(Self::DiscoverSources),
            Self::ScanInstallations => Some(Self::ConfirmCatalogs),
            Self::Summary => Some(Self::ScanInstallations),
        }
    }

    pub(crate) fn number(self) -> usize {
        match self {
            Self::Welcome => 1,
            Self::DetectAgents => 2,
            Self::ChooseScanRoots => 3,
            Self::DiscoverSources => 4,
            Self::ConfirmCatalogs => 5,
            Self::ScanInstallations => 6,
            Self::Summary => 7,
        }
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Welcome => "Welcome and scope",
            Self::DetectAgents => "Detect agents",
            Self::ChooseScanRoots => "Choose scan roots",
            Self::DiscoverSources => "Discover sources",
            Self::ConfirmCatalogs => "Confirm catalogs",
            Self::ScanInstallations => "Scan installations",
            Self::Summary => "Summary",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum View {
    Setup(SetupStep),
    Inventory,
    Sources,
    Updates,
    Doctor,
    Settings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdatesPane {
    Candidates,
    Details,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourcesPane {
    Repositories,
    Variants,
    Details,
}

/// The regions of the Inventory workspace, in reading order.
///
/// A wide terminal shows both at once and this is only focus; a compact one
/// shows the focused region alone, so advancing is a drill-in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InventoryPane {
    Skills,
    Details,
}

/// The regions of the Doctor workspace, in reading order.
///
/// The same shape as [`InventoryPane`]: a wide terminal shows both and this is
/// only focus; a compact one shows the focused region alone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DoctorPane {
    Findings,
    Details,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Continue,
    Back,
    MoveSelection(i8),
    ToggleSelection,
    OpenHelp,
    CloseHelp,
    OpenSettings,
    OpenInventory,
    OpenSources,
    OpenUpdates,
    OpenDoctor,
    MoveDoctorPane(i8),
    AdvanceDoctorPane,
    MoveDoctorSelection(i8),
    BeginAddSource,
    AppendSourcePath(char),
    DeleteSourcePathCharacter,
    SubmitSourcePath,
    CancelSourceFlow,
    MoveCatalogSelection(i8),
    ToggleCatalogIncluded,
    ToggleCatalogClassification,
    ToggleCatalogCompatibility(AgentKind),
    ConfirmPendingSource,
    MoveSourcesPane(i8),
    AdvanceSourcesPane,
    MoveSourcesSelection(i8),
    MoveUpdatesPane(i8),
    AdvanceUpdatesPane,
    MoveUpdatesSelection(i8),
    BeginUpdateCheck,
    CancelUpdateCheck,
    BeginRepositoryUpdate,
    ConfirmRepositoryUpdate,
    DismissRepositoryUpdate,
    MoveInventoryPane(i8),
    AdvanceInventoryPane,
    MoveInventorySelection(i8),
    /// Move the focused detail region's window by lines.
    ///
    /// Named apart from the selection movements because it moves a viewport
    /// rather than a selection: the same keys do both, in different regions,
    /// and a reducer test that could not tell them apart would be reading the
    /// key rather than the behaviour. One action serves every screen that has
    /// a detail region, because only one is ever on screen.
    ScrollDetail(i8),
    BeginInventoryFilter,
    AppendInventoryFilter(char),
    DeleteInventoryFilterCharacter,
    SubmitInventoryFilter,
    /// Plan installing the focused variant, and show what it would do.
    ///
    /// Nothing is written by this, and nothing is written by anything until
    /// [`Action::ConfirmOperation`] is applied to the preview it produces.
    BeginInstall,
    /// Plan removal of the focused skill's owned links.
    BeginUninstall,
    BeginForgetSource,
    ConfirmOperation,
    DismissOperation,
    /// Plan repairing the selected Doctor observation. Planning is read-only.
    BeginRepair,
    ConfirmRepair,
    DismissRepair,
    RerunSetup,
    Quit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateOutcome {
    Continue,
    Quit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    PersistSetup {
        agent_selections: [bool; 3],
    },
    ResetSetup,
    RedetectAgents {
        agent_selections: [bool; 3],
    },
    InspectSource {
        path: PathBuf,
    },
    RegisterSource {
        preview: SourcePreview,
    },
    ScanInstallations,
    /// Read the machine and build an install preview for the focused variant.
    ///
    /// The variant is not carried on the effect: the reducer decided that this
    /// is what the user is standing on, and the runner reads it back from the
    /// same state the reducer read, exactly as [`Effect::ScanInstallations`]
    /// carries no roots.
    PlanInstall,
    /// Create the links the shown preview calls work, then rescan and verify.
    ApplyInstall,
    PlanUninstall,
    ApplyUninstall,
    PlanForgetSource,
    ApplyForgetSource,
    /// Read the machine and build a repair preview for the selected finding.
    PlanRepair,
    /// Replace the link in the shown repair preview, then rescan and verify.
    ApplyRepair,
    CheckUpdates,
    CancelUpdateCheck,
    RecordUpdateChecks(Vec<CachedUpdateCheck>),
    FinishUpdateCheck,
    PlanRepositoryUpdate,
    ApplyRepositoryUpdate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateResult {
    outcome: UpdateOutcome,
    effects: Vec<Effect>,
}

struct UpdateCheckRun {
    receiver: Receiver<UpdateCheckMessage>,
    handle: JoinHandle<()>,
    cancelled: Arc<AtomicBool>,
    terminal_state: Arc<AtomicU8>,
    child: Arc<Mutex<Option<Child>>>,
}

const UPDATE_CHECK_RUNNING: u8 = 0;
const UPDATE_CHECK_CANCELLED: u8 = 1;
const UPDATE_CHECK_FINISHED: u8 = 2;

pub(crate) struct RepositoryApplyOutcome {
    pub(crate) verification: Option<crate::updates::RepositoryVerifyReport>,
    pub(crate) apply_error: Option<String>,
    pub(crate) bookkeeping_error: Option<String>,
    pub(crate) write_attempted: bool,
}

enum UpdateCheckMessage {
    Progress { completed: usize, total: usize },
    Finished(Vec<CachedUpdateCheck>),
    Cancelled,
    Failed(String),
}

impl UpdateResult {
    fn continuing(effects: Vec<Effect>) -> Self {
        Self {
            outcome: UpdateOutcome::Continue,
            effects,
        }
    }

    fn quit_with(effects: Vec<Effect>) -> Self {
        Self {
            outcome: UpdateOutcome::Quit,
            effects,
        }
    }

    pub fn outcome(&self) -> UpdateOutcome {
        self.outcome
    }

    pub fn effects(&self) -> &[Effect] {
        &self.effects
    }
}

/// The longest inventory filter query Skilled will hold.
///
/// The query is echoed in the workspace header, so an unbounded one would wrap
/// until it squeezed the table it is meant to narrow down to nothing. No skill
/// name approaches this length.
pub(crate) const MAX_INVENTORY_FILTER: usize = 128;

/// Step a selection through a list, wrapping at both ends.
///
/// The arithmetic stays in `usize` because a list long enough to overflow a
/// narrower index would silently wrap into a zero modulus and panic.
fn wrapped_index(current: usize, delta: i8, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let magnitude = usize::from(delta.unsigned_abs()) % count;
    if delta >= 0 {
        (current + magnitude) % count
    } else {
        (current + count - magnitude) % count
    }
}

/// The highest generation this process has handed out or seen recorded.
static GENERATION: AtomicI64 = AtomicI64::new(0);

/// The wall clock, as the nanosecond value a generation starts from.
fn wall_clock() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(i64::MAX as u128) as i64
}

/// When a check ran, as a value later checks are guaranteed to exceed.
///
/// The conditional upsert behind a recorded check keeps an older result from
/// overwriting a newer one, so this value orders writes as much as it dates
/// them, and wall time alone cannot do that: an NTP correction, a manual clock
/// change, or a restored virtual machine moves it backwards, and every explicit
/// check made before it caught up would be dropped by a store that reported
/// success. The clock still supplies the value — Updates states when a check
/// ran — but it is never allowed to fall to or below one already handed out.
/// [`note_generation`] seeds the floor from what the store already holds, so a
/// clock that moved back between runs cannot silence the next run's first check.
///
/// This orders one process against itself. Ordering it against another Skilled
/// process is the store's job, because only shared state can do it:
/// [`SkilledApp::reserve_generations`] allocates from
/// [`Store::reserve_update_check_generations`] and falls back here only when
/// that reservation cannot be made at all.
fn now() -> i64 {
    let wall = wall_clock();
    let mut floor = GENERATION.load(Ordering::Acquire);
    loop {
        let next = wall.max(floor.saturating_add(1));
        match GENERATION.compare_exchange_weak(floor, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return next,
            Err(current) => floor = current,
        }
    }
}

/// Raise the generation floor to a value the store already holds.
fn note_generation(generation: i64) {
    GENERATION.fetch_max(generation, Ordering::AcqRel);
}

/// One row the variants pane offers the selection.
///
/// Every rendered row of that pane is a focus position, and three things need
/// to agree on which rows exist and in what order: the count the selection
/// wraps at, the renderer, and whatever the detail region says the selection
/// rests on. [`variant_rows`] is the one statement of that order, so those
/// three are a `.count()`, a `map`, and an `.nth()` over the same sequence
/// rather than three loops held together by a `debug_assert` that compiles out
/// of release builds.
///
/// A catalog's state row is kept distinct from a variant because the pane
/// draws each differently — an unreadable catalog is not an empty one — while
/// the detail region treats both as naming their catalog.
#[derive(Clone, Copy)]
pub enum SourceRow<'a> {
    /// A catalog that could not be scanned, and what went wrong.
    ///
    /// The message is carried rather than looked up again from the catalog,
    /// so no site that builds one of these can omit it: a renderer that had to
    /// ask for it a second time would need something to draw when the answer
    /// came back `None`, and an `unavailable` badge beside an empty message
    /// states a failure while withholding what it was. [`catalog_rows`] is the
    /// only place in the crate that builds this row, and it takes both from
    /// the same catalog.
    CatalogError {
        catalog: &'a CatalogProposal,
        error: &'a str,
    },
    /// A skill candidate, and the catalog it was found in.
    Variant {
        catalog: &'a CatalogProposal,
        candidate: &'a SkillCandidate,
    },
    /// A catalog that was read and holds nothing.
    NoVariants(&'a CatalogProposal),
}

impl<'a> SourceRow<'a> {
    /// The catalog the row belongs to, whichever kind it is.
    pub fn catalog(self) -> &'a CatalogProposal {
        match self {
            Self::CatalogError { catalog, .. }
            | Self::Variant { catalog, .. }
            | Self::NoVariants(catalog) => catalog,
        }
    }
}

/// The rows one catalog contributes: its scan error, then its candidates, then
/// the `no variants` line an empty catalog shows.
///
/// The error leads because it is why the rows beneath it may be missing. A
/// catalog that failed to scan yields no candidates today, so the two never
/// appear together and the order between them is only a promise about where
/// the error would sit; the sequence is written to hold either way rather
/// than to assume the scanner keeps them exclusive.
///
/// `no variants` is reached only by a catalog that was read cleanly and holds
/// nothing: an unreadable catalog says it is unreadable instead of claiming it
/// is empty, which is a distinction the scan keeps apart everywhere else.
pub(crate) fn catalog_rows(catalog: &CatalogProposal) -> impl Iterator<Item = SourceRow<'_>> {
    let error_row = catalog
        .scan_error()
        .map(|error| SourceRow::CatalogError { catalog, error });
    let empty_row = (catalog.scan_error().is_none() && catalog.candidates().is_empty())
        .then_some(SourceRow::NoVariants(catalog));
    error_row
        .into_iter()
        .chain(
            catalog
                .candidates()
                .iter()
                .map(move |candidate| SourceRow::Variant { catalog, candidate }),
        )
        .chain(empty_row)
}

/// Every row of the variants pane, in the order the pane draws them.
///
/// A source that could not be read at all yields no rows, so there is nothing
/// there to select. What the pane puts in their place is the pane's own
/// decision — it renders the source error and returns before it reaches this
/// sequence — and the two agree on the condition rather than on the outcome.
pub fn variant_rows(source: &RegisteredSource) -> impl Iterator<Item = SourceRow<'_>> {
    let catalogs = match source.source_error() {
        Some(_) => &[][..],
        None => source.catalogs(),
    };
    catalogs.iter().flat_map(catalog_rows)
}

/// Why no install plan could be built.
///
/// The two are kept apart because a caller acts on them differently: a request
/// Skilled cannot honour is the user's to correct, and metadata Skilled cannot
/// read is not something a different request would fix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlanRequestFailure {
    /// The request names no variant Skilled can plan an install for.
    Unplannable(String),
    /// Skilled's own metadata could not be read.
    Metadata(MetadataFailure),
}

#[derive(Clone, Debug)]
pub enum DoctorItem<'a> {
    Installation(DoctorEntry<'a>),
    Source {
        source: &'a RegisteredSource,
        check: &'a CachedUpdateCheck,
        finding: Finding,
    },
}

impl<'a> DoctorItem<'a> {
    pub fn skill_name(&self) -> &str {
        match self {
            Self::Installation(entry) => entry.skill_name(),
            Self::Source { source, .. } => source.label(),
        }
    }
    pub fn finding(&self) -> &Finding {
        match self {
            Self::Installation(entry) => entry.finding(),
            Self::Source { finding, .. } => finding,
        }
    }
    /// Source findings have no agent dimension, so callers must handle the
    /// absence rather than attributing a repository state to an agent.
    pub fn agent(&self) -> Option<AgentKind> {
        self.agent_option()
    }
    pub fn agent_option(&self) -> Option<AgentKind> {
        match self {
            Self::Installation(entry) => Some(entry.agent()),
            Self::Source { .. } => None,
        }
    }
    pub fn observation(&self) -> Option<&'a InstalledSkillObservation> {
        match self {
            Self::Installation(entry) => entry.observation(),
            Self::Source { .. } => None,
        }
    }
    pub fn variants(&self) -> &'a [VariantRef] {
        match self {
            Self::Installation(entry) => entry.variants(),
            Self::Source { .. } => &[],
        }
    }
    pub fn concerns_the_registry(&self) -> bool {
        matches!(self, Self::Installation(entry) if entry.concerns_the_registry())
    }
    pub fn source(&self) -> Option<&'a RegisteredSource> {
        match self {
            Self::Source { source, .. } => Some(source),
            _ => None,
        }
    }
    pub fn check(&self) -> Option<&'a CachedUpdateCheck> {
        match self {
            Self::Source { check, .. } => Some(check),
            _ => None,
        }
    }
}

impl PlanRequestFailure {
    pub fn message(&self) -> String {
        match self {
            Self::Unplannable(message) => message.clone(),
            Self::Metadata(failure) => failure.to_string(),
        }
    }
}

enum Metadata {
    Ready(Store),
    Unavailable(MetadataFailure),
}

/// Why a source did not become registered.
///
/// The two answers lead to different sessions, so they are not one type. A
/// store that failed leaves nothing further writable and degrades the session
/// for good; a request the store refused leaves the metadata exactly as usable
/// as it was, and belongs to the flow that made it — the same place a failed
/// revalidation is already reported.
enum RegistrationFailure {
    Request(Error),
    Metadata(MetadataFailure),
}

/// Whether an error names the request rather than the store behind it.
///
/// A checkout path this build cannot represent in the metadata is a fact about
/// the path: it is refused before anything is written, and refusing it says
/// nothing about whether the next path could be registered.
fn is_source_request_error(error: &Error) -> bool {
    matches!(error, Error::InvalidSourcePath(_))
}

struct MetadataStartup {
    metadata: Metadata,
    setup_complete: Option<bool>,
    agent_selections: Option<[bool; 3]>,
    sources: Vec<RegisteredSource>,
    update_checks: Vec<CachedUpdateCheck>,
    receipts: Option<Vec<Receipt>>,
    registry_availability: RegistryAvailability,
}

fn open_metadata(data_dir: &Path) -> MetadataStartup {
    let database_path = data_dir.join("skilled.sqlite3");
    match Store::open(data_dir) {
        Ok(store) => {
            // Completion, scan scope, and sources are independent recovery
            // units. One bad value must not discard another that was read.
            let setup_complete = store.setup_complete();
            let agent_selections = store.agent_selections();
            let sources = store.load_registered_sources(false);
            let update_checks = store.update_checks();
            let receipts = store.receipts();
            let inconsistent_setup =
                matches!((&setup_complete, &agent_selections), (Ok(true), Ok(None))).then(|| {
                    Error::InvalidSetupMetadata(
                        "setup_complete is true but configured_agents is empty".to_owned(),
                    )
                });
            let failure = setup_complete
                .as_ref()
                .err()
                .map(ToString::to_string)
                .or_else(|| agent_selections.as_ref().err().map(ToString::to_string))
                .or_else(|| inconsistent_setup.as_ref().map(ToString::to_string))
                .or_else(|| sources.as_ref().err().map(ToString::to_string))
                .or_else(|| update_checks.as_ref().err().map(ToString::to_string))
                .or_else(|| {
                    receipts
                        .as_ref()
                        .err()
                        .map(|error| format!("ownership receipts could not be read: {error}"))
                })
                // Last, so a value that is actually invalid leads: a store
                // that opened read-only is a reason this session cannot
                // write, not a reason to distrust anything it just read.
                .or_else(|| {
                    store
                        .read_only()
                        .then(|| Error::ReadOnlyMetadata.to_string())
                })
                .map(|error| MetadataFailure::new(database_path, error.to_string()));
            let registry_availability = if sources.is_ok() {
                RegistryAvailability::Readable
            } else {
                RegistryAvailability::Unavailable
            };
            MetadataStartup {
                metadata: match failure {
                    Some(failure) => Metadata::Unavailable(failure),
                    None => Metadata::Ready(store),
                },
                setup_complete: setup_complete.ok(),
                agent_selections: agent_selections.ok().flatten(),
                sources: sources.unwrap_or_default(),
                update_checks: update_checks.unwrap_or_default(),
                receipts: receipts.ok(),
                registry_availability,
            }
        }
        Err(error) => MetadataStartup {
            metadata: Metadata::Unavailable(MetadataFailure::new(database_path, error.to_string())),
            setup_complete: None,
            agent_selections: None,
            sources: Vec::new(),
            update_checks: Vec::new(),
            receipts: None,
            registry_availability: RegistryAvailability::Unavailable,
        },
    }
}

pub struct SkilledApp {
    view: View,
    metadata: Metadata,
    registry_availability: RegistryAvailability,
    scan_scope_known: bool,
    environment: AppEnvironment,
    agents: [AgentDetection; 3],
    focused_agent: usize,
    sources: Vec<RegisteredSource>,
    source_path: String,
    source_path_input_active: bool,
    pending_source: Option<SourcePreview>,
    source_error: Option<String>,
    focused_catalog: usize,
    sources_pane: SourcesPane,
    focused_source: usize,
    focused_variant: usize,
    update_checks: Vec<CachedUpdateCheck>,
    updates_pane: UpdatesPane,
    focused_update: usize,
    pending_update: Option<RepositoryUpdatePrompt>,
    update_preview_fully_seen: bool,
    update_check_run: Option<UpdateCheckRun>,
    retired_update_workers: Vec<JoinHandle<()>>,
    update_check_progress: Option<(usize, usize)>,
    update_check_error: Option<String>,
    inventory: InventorySnapshot,
    /// Receipt-aware offers and findings recomputed with every inventory scan.
    repair_overlay: RepairOverlay,
    /// Indices into `inventory.rows()` that the filter admits.
    ///
    /// Recomputed whenever the snapshot or the query changes rather than on
    /// every frame, because rendering and the key-hint bar both ask for it.
    filtered_installations: Vec<usize>,
    inventory_pane: InventoryPane,
    focused_installation: usize,
    inventory_filter: String,
    inventory_filter_active: bool,
    /// Lines of the detail region's content scrolled past the top of its body.
    ///
    /// Lines, not rows: the region states observed fields, and a window that
    /// opened or closed inside a wrapped one would show a path without its
    /// label or a label without its path. What the region *reports* is still
    /// counted in rows, which is what a reader loses.
    detail_scroll: usize,
    /// The furthest offset the last drawn frame found useful, and the only
    /// bound the reducer has: `update` never learns the terminal's size, so
    /// the renderer measures the region and the runner notes what it found.
    detail_max_scroll: usize,
    /// Whether the last frame measured the scrollable region at all.
    ///
    /// A frame that drew nothing — a terminal below the minimum size — measured
    /// nothing, which is not the same as measuring zero. Kept apart because a
    /// confirmation waits on a plan having been on screen, and a stale extent
    /// would answer for a frame the reader never saw.
    detail_measured: bool,
    doctor_pane: DoctorPane,
    focused_finding: usize,
    /// The operation dialog, when one is open.
    ///
    /// While it is set it owns the keyboard, the way the help overlay and the
    /// catalog confirmation do: a preview is a question, and a stray navigation
    /// key must not answer it.
    pending_operation: Option<OperationPrompt>,
    /// The repair dialog, mutually exclusive with the operation dialog.
    pending_repair: Option<RepairPrompt>,
    /// Last readable ownership snapshot. `None` means Skilled could not tell.
    cached_receipts: Option<Vec<Receipt>>,
    help_context: Option<View>,
}

impl SkilledApp {
    pub fn open(environment: AppEnvironment) -> Result<Self> {
        let startup = open_metadata(&environment.data_dir);
        let view = match (&startup.metadata, startup.setup_complete) {
            (Metadata::Ready(_), Some(true)) => View::Inventory,
            (Metadata::Ready(_), Some(false)) => View::Setup(SetupStep::Welcome),
            (Metadata::Ready(_), None) => unreachable!("ready metadata has setup completion"),
            (Metadata::Unavailable(_), _) => View::Inventory,
        };
        let registry_availability = startup.registry_availability;
        let mut agents = detect_agents(&environment);
        let agent_selections = startup.agent_selections;
        if let Some(selections) = agent_selections {
            for (agent, selected) in agents.iter_mut().zip(selections) {
                agent.set_selected(selected);
            }
        }
        let sources = startup.sources;
        let update_checks = startup.update_checks;
        for check in &update_checks {
            note_generation(check.checked_at);
        }
        // Setup reads the installation roots at its own step, after the user
        // has chosen which agents Skilled should configure. Reading them
        // before that would look at roots the user may be about to deselect.
        // Once setup is complete the selections are known, so opening
        // straight into the Inventory opens onto a real scan.
        let inventory = if view == View::Inventory {
            scan_installations(&agents, &sources, registry_availability)
        } else {
            InventorySnapshot::not_scanned(&agents, registry_availability)
        };
        let mut cached_receipts: Option<Vec<Receipt>> = None;
        let startup_receipts = match startup.receipts {
            Some(receipts) => Ok(receipts),
            None => match &startup.metadata {
                Metadata::Unavailable(failure) => Err(Error::MetadataUnavailable(failure.clone())),
                Metadata::Ready(_) => unreachable!("ready metadata has readable receipts"),
            },
        };
        let repair_overlay = match startup_receipts {
            Ok(receipts) => {
                let overlay = RepairOverlay::build(&inventory, &receipts, &sources, &agents);
                cached_receipts = Some(receipts);
                overlay
            }
            // A degraded session names its database in the banner above every
            // screen. Repeating the path inside a detail field would say the
            // same thing twice, in the narrowest column on the screen.
            Err(Error::MetadataUnavailable(_)) => RepairOverlay::receipts_unread(
                "the application metadata is unavailable this session".to_owned(),
            ),
            Err(error) => RepairOverlay::receipts_unread(error.to_string()),
        };
        let mut app = Self {
            view,
            metadata: startup.metadata,
            registry_availability,
            scan_scope_known: agent_selections.is_some(),
            environment,
            agents,
            focused_agent: 0,
            sources,
            source_path: String::new(),
            source_path_input_active: false,
            pending_source: None,
            source_error: None,
            focused_catalog: 0,
            sources_pane: SourcesPane::Repositories,
            focused_source: 0,
            focused_variant: 0,
            update_checks,
            updates_pane: UpdatesPane::Candidates,
            focused_update: 0,
            pending_update: None,
            update_preview_fully_seen: false,
            update_check_run: None,
            retired_update_workers: Vec::new(),
            update_check_progress: None,
            update_check_error: None,
            inventory,
            repair_overlay,
            filtered_installations: Vec::new(),
            inventory_pane: InventoryPane::Skills,
            focused_installation: 0,
            inventory_filter: String::new(),
            inventory_filter_active: false,
            detail_scroll: 0,
            detail_max_scroll: 0,
            detail_measured: false,
            doctor_pane: DoctorPane::Findings,
            focused_finding: 0,
            pending_operation: None,
            pending_repair: None,
            cached_receipts,
            help_context: None,
        };
        app.refilter_installations();
        Ok(app)
    }

    pub fn view(&self) -> View {
        self.view
    }

    /// The database every metadata failure names, whether or not it is open.
    fn metadata_database_path(&self) -> PathBuf {
        match &self.metadata {
            Metadata::Ready(store) => store.database_path().to_path_buf(),
            Metadata::Unavailable(failure) => failure.database_path().to_path_buf(),
        }
    }

    /// The metadata store, or the failure that took it away.
    ///
    /// A degraded session keeps everything it read before the failure, so the
    /// callers that only *read* still have data to work from. Every caller
    /// that needs the store itself asks here and is answered with the failure
    /// rather than with an empty result that would read as an answer.
    fn store(&self) -> Result<&Store> {
        match &self.metadata {
            Metadata::Ready(store) => Ok(store),
            Metadata::Unavailable(failure) => Err(Error::MetadataUnavailable(failure.clone())),
        }
    }

    fn store_mut(&mut self) -> Result<&mut Store> {
        match &mut self.metadata {
            Metadata::Ready(store) => Ok(store),
            Metadata::Unavailable(failure) => {
                let failure = failure.clone();
                Err(Error::MetadataUnavailable(failure))
            }
        }
    }

    pub fn metadata_failure(&self) -> Option<&MetadataFailure> {
        match &self.metadata {
            Metadata::Ready(_) => None,
            Metadata::Unavailable(failure) => Some(failure),
        }
    }

    pub fn registry_availability(&self) -> RegistryAvailability {
        self.registry_availability
    }

    pub(crate) fn scan_scope_known(&self) -> bool {
        self.scan_scope_known
    }

    #[cfg(test)]
    fn fail_metadata_next(&self, operation: crate::store::MetadataOperation) {
        let Metadata::Ready(store) = &self.metadata else {
            panic!("metadata is already unavailable");
        };
        store.fail_next(operation);
    }

    pub fn agent(&self, kind: AgentKind) -> &AgentDetection {
        detection_at(&self.agents, kind)
    }

    pub fn agents(&self) -> &[AgentDetection; 3] {
        &self.agents
    }

    pub fn focused_agent(&self) -> usize {
        self.focused_agent
    }

    pub fn sources(&self) -> &[RegisteredSource] {
        &self.sources
    }

    pub fn update_checks(&self) -> &[CachedUpdateCheck] {
        &self.update_checks
    }

    pub fn updates_pane(&self) -> UpdatesPane {
        self.updates_pane
    }

    pub fn focused_update(&self) -> usize {
        self.focused_update
    }

    pub fn pending_update(&self) -> Option<&RepositoryUpdatePrompt> {
        self.pending_update.as_ref()
    }

    pub fn update_preview_fully_seen(&self) -> bool {
        self.update_preview_fully_seen
    }

    pub fn update_check_in_flight(&self) -> bool {
        self.update_check_run.is_some()
    }

    pub fn update_check_progress(&self) -> Option<(usize, usize)> {
        self.update_check_progress
    }

    pub fn update_check_error(&self) -> Option<&str> {
        self.update_check_error.as_deref()
    }

    pub fn drain_update_check(&mut self) -> Vec<Effect> {
        self.reap_retired_update_workers();
        let Some(run) = self.update_check_run.as_mut() else {
            return Vec::new();
        };
        let mut effects = Vec::new();
        loop {
            match run.receiver.try_recv() {
                Ok(UpdateCheckMessage::Progress { completed, total }) => {
                    self.update_check_progress = Some((completed, total));
                }
                Ok(UpdateCheckMessage::Finished(checks)) => {
                    effects.extend([
                        Effect::RecordUpdateChecks(checks),
                        Effect::FinishUpdateCheck,
                    ]);
                    break;
                }
                Ok(UpdateCheckMessage::Failed(error)) => {
                    self.update_check_error = Some(error);
                    effects.push(Effect::FinishUpdateCheck);
                    break;
                }
                Ok(UpdateCheckMessage::Cancelled) => {
                    effects.push(Effect::FinishUpdateCheck);
                    break;
                }
                Err(TryRecvError::Disconnected) => {
                    if !run.cancelled.load(Ordering::Acquire) {
                        self.update_check_error =
                            Some("update check ended before completing".to_owned());
                    }
                    effects.push(Effect::FinishUpdateCheck);
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        effects
    }

    fn reap_retired_update_workers(&mut self) {
        let mut index = 0;
        while index < self.retired_update_workers.len() {
            if self.retired_update_workers[index].is_finished() {
                let handle = self.retired_update_workers.swap_remove(index);
                let _ = handle.join();
            } else {
                index += 1;
            }
        }
    }

    /// Latch the renderer's statement-only confirmation measurement.
    ///
    /// Changed-file evidence continues below the statement and remains fully
    /// scrollable, but it does not become a second consent gate.
    pub fn note_update_preview_fully_seen(&mut self, seen: Option<bool>) {
        self.update_preview_fully_seen |= seen.unwrap_or(false);
    }

    pub fn selected_update_source(&self) -> Option<&RegisteredSource> {
        self.sources.get(self.focused_update)
    }

    pub fn update_check_for(&self, source_id: i64) -> Option<&CachedUpdateCheck> {
        self.update_checks
            .iter()
            .find(|check| check.source_id == source_id)
    }

    /// Count available updates only when the result covers the whole current
    /// registry. A partial count would turn "checked some" into "none left".
    pub fn stated_update_count(&self) -> Option<usize> {
        (!self.sources.is_empty()
            && self.sources.iter().all(|source| {
                self.update_check_for(source.id())
                    .is_some_and(|check| !check.superseded_by(source) && check.availability_known())
            }))
        .then(|| {
            self.sources
                .iter()
                .filter(|source| {
                    self.update_check_for(source.id())
                        .is_some_and(|check| check.verdict == RepositoryUpdateVerdict::Available)
                })
                .count()
        })
    }

    pub fn source_path(&self) -> &str {
        &self.source_path
    }

    pub fn source_path_input_active(&self) -> bool {
        self.source_path_input_active
    }

    pub fn pending_source(&self) -> Option<&SourcePreview> {
        self.pending_source.as_ref()
    }

    pub fn source_error(&self) -> Option<&str> {
        self.source_error.as_deref()
    }

    pub fn focused_catalog(&self) -> usize {
        self.focused_catalog
    }

    pub fn sources_pane(&self) -> SourcesPane {
        self.sources_pane
    }

    pub fn focused_source(&self) -> usize {
        self.focused_source
    }

    pub fn focused_variant(&self) -> usize {
        self.focused_variant
    }

    /// How many rows the variants pane offers the selection.
    ///
    /// Counting [`variant_rows`] rather than re-deriving the tally is what
    /// keeps a list taller than the pane walkable whatever mixture of skills,
    /// errors, and empty catalogs it holds: the pane draws exactly the rows
    /// this counts, because both are the same sequence.
    pub fn variants_row_count(&self) -> usize {
        self.selected_source()
            .map(variant_rows)
            .map_or(0, Iterator::count)
    }

    /// The row the variants-pane selection rests on, or `None` when the pane
    /// offers no rows to rest on.
    pub fn selected_variant_row(&self) -> Option<SourceRow<'_>> {
        variant_rows(self.selected_source()?).nth(self.focused_variant)
    }

    pub fn help_context(&self) -> Option<View> {
        self.help_context
    }

    pub fn pending_operation(&self) -> Option<&OperationPrompt> {
        self.pending_operation.as_ref()
    }

    /// Project the open operation to its install prompt, when it is one.
    ///
    /// `None` does not mean no operation dialog is open; callers deciding
    /// keyboard ownership must use [`Self::pending_operation`].
    pub fn pending_install(&self) -> Option<&InstallPrompt> {
        match self.pending_operation.as_ref() {
            Some(OperationPrompt::Install(prompt)) => Some(prompt),
            Some(OperationPrompt::Uninstall(_) | OperationPrompt::Forget(_)) | None => None,
        }
    }

    pub fn cached_receipts(&self) -> Option<&[Receipt]> {
        self.cached_receipts.as_deref()
    }

    /// Whether the selected inventory row contains a link Skilled may own.
    pub fn can_uninstall_selection(&self) -> bool {
        if self.metadata_failure().is_some() || self.view != View::Inventory {
            return false;
        }
        let Some(row) = self.selected_installation() else {
            return false;
        };
        AgentKind::ALL.into_iter().any(|agent| {
            let Some(observation) = row.observation(agent) else {
                return false;
            };
            let InstallationObject::Symlink { target } = observation.object() else {
                return false;
            };
            self.cached_receipts.as_ref().is_none_or(|receipts| {
                receipts.iter().any(|receipt| {
                    receipt.agent() == agent
                        && receipt.link_path() == observation.path()
                        && receipt.link_target() == target
                })
            })
        })
    }

    pub fn can_forget_source(&self) -> bool {
        self.metadata_failure().is_none()
            && self.view == View::Sources
            && self.sources_pane == SourcesPane::Repositories
            && self.selected_source().is_some()
    }

    pub fn pending_repair(&self) -> Option<&RepairPrompt> {
        self.pending_repair.as_ref()
    }

    pub fn can_repair_selection(&self) -> bool {
        self.view == View::Doctor
            && self
                .selected_finding()
                .as_ref()
                .is_some_and(|entry| self.can_repair_finding(entry))
    }

    /// Whether one already-materialised Doctor entry offers repair.
    ///
    /// The renderer orders the Doctor list once per frame and uses this form
    /// for its key hint and help entry. Asking [`Self::can_repair_selection`]
    /// there would materialise and sort the same bounded list again.
    ///
    /// A repository finding has no observed installation behind it, so it
    /// offers nothing here: repair replaces one link Skilled is proven to own.
    pub fn can_repair_finding(&self, entry: &DoctorItem<'_>) -> bool {
        self.metadata_failure().is_none()
            && self.view == View::Doctor
            && entry
                .observation()
                .is_some_and(|observation| self.repair_overlay.is_offered(observation.path()))
    }

    pub fn repair_offer(&self, path: &Path) -> RepairOfferStatus {
        self.repair_overlay.offer(path)
    }

    pub fn repair_overlay_finding(&self, path: &Path) -> Option<&crate::inventory::Finding> {
        self.repair_overlay.finding_at(path)
    }

    /// Whether the focused row is one the install flow would act on.
    ///
    /// The variants pane and the detail region beside it both stand on a row;
    /// the repositories pane stands on a source, and its variant selection is
    /// whatever it was left at, which is not something the user is looking at.
    ///
    /// A candidate that does not validate is not one any agent would resolve to
    /// — `resolution::select_candidates` drops it — so there is nothing to
    /// install from it, and offering the key would promise an answer whose only
    /// content is that the row was never installable.
    pub fn can_install_selection(&self) -> bool {
        self.metadata_failure().is_none()
            && self.view == View::Sources
            && matches!(
                self.sources_pane,
                SourcesPane::Variants | SourcesPane::Details
            )
            && matches!(
                self.selected_variant_row(),
                Some(SourceRow::Variant { candidate, .. }) if candidate.validation().is_valid()
            )
    }

    pub fn can_add_source(&self) -> bool {
        self.metadata_failure().is_none()
    }

    pub fn can_rerun_setup(&self) -> bool {
        self.metadata_failure().is_none()
    }

    /// Every ownership receipt Skilled holds.
    ///
    /// Spec 7 evidence, exposed so a caller can see what Skilled claims to have
    /// put on disk. Nothing reads one as an instruction.
    pub fn receipts(&self) -> Result<Vec<Receipt>> {
        match &self.metadata {
            Metadata::Ready(store) => store.receipts(),
            Metadata::Unavailable(failure) => Err(Error::MetadataUnavailable(failure.clone())),
        }
    }

    pub fn inventory(&self) -> &InventorySnapshot {
        &self.inventory
    }

    /// The home directory every agent root hangs off.
    ///
    /// Screens abbreviate installation paths against it, because a global
    /// skill root is only ever spoken about as `~/.claude/skills`.
    pub fn home(&self) -> &Path {
        &self.environment.home_dir
    }

    /// Who and where this session is, as gathered once at startup; the
    /// reducer never reads the process environment itself.
    pub fn identity(&self) -> &SessionIdentity {
        &self.environment.identity
    }

    pub fn doctor_pane(&self) -> DoctorPane {
        self.doctor_pane
    }

    pub fn focused_finding(&self) -> usize {
        self.focused_finding
    }

    /// Every finding the last scan holds, in the order Doctor lists them.
    ///
    /// Materialised on demand from the snapshot, its receipt-aware repair
    /// overlay, and the cached update checks. Nothing is held beside those, so
    /// no list of findings can disagree with them after a rescan or a check.
    pub fn doctor_findings(&self) -> Vec<DoctorItem<'_>> {
        let mut items: Vec<_> = self
            .inventory
            .doctor_findings()
            .map(DoctorItem::Installation)
            .collect();
        items.extend(self.repair_overlay.findings().iter().filter_map(|overlay| {
            let row = self.inventory.rows().get(overlay.row_index())?;
            let observation = row.observation(overlay.agent())?;
            debug_assert_eq!(observation.path(), overlay.path());
            Some(DoctorItem::Installation(DoctorEntry::from_observation(
                row.name(),
                overlay.finding(),
                observation,
            )))
        }));
        items.extend(self.sources.iter().flat_map(|source| {
            let Some(check) = self.update_check_for(source.id()) else {
                return Vec::new().into_iter();
            };
            if check.superseded_by(source) {
                return Vec::new().into_iter();
            }
            check
                .findings()
                .into_iter()
                .map(|finding| DoctorItem::Source {
                    source,
                    check,
                    finding,
                })
                .collect::<Vec<_>>()
                .into_iter()
        }));
        items.sort_by_key(|item| {
            (
                doctor_order(item.finding().code()),
                std::cmp::Reverse(item.finding().severity()),
                item.skill_name().to_owned(),
                item.agent_option().map_or(0, |agent| agent.index() + 1),
            )
        });
        items
    }

    pub fn selected_finding(&self) -> Option<DoctorItem<'_>> {
        self.doctor_findings().into_iter().nth(self.focused_finding)
    }

    /// How many findings the last scan holds, without ordering them.
    ///
    /// The key-hint bar and the navigation row ask this on every frame of every
    /// view, so neither may pay for the sort that presenting them costs.
    pub fn finding_count(&self) -> usize {
        self.inventory.finding_count()
            + self.repair_overlay.finding_count()
            + self
                .sources
                .iter()
                .map(|source| {
                    self.update_check_for(source.id()).map_or(0, |check| {
                        if check.superseded_by(source) {
                            0
                        } else {
                            check.findings().len()
                        }
                    })
                })
                .sum::<usize>()
    }

    pub fn stated_finding_count(&self) -> Option<usize> {
        (self.repair_overlay.receipts_readable())
            .then(|| self.inventory.stated_finding_count())
            .flatten()
            .map(|_| self.finding_count())
    }

    pub fn repair_receipts_readable(&self) -> bool {
        self.repair_overlay.receipts_readable()
    }

    pub fn inventory_pane(&self) -> InventoryPane {
        self.inventory_pane
    }

    pub fn focused_installation(&self) -> usize {
        self.focused_installation
    }

    pub fn inventory_filter(&self) -> &str {
        &self.inventory_filter
    }

    /// How far the detail region's window has been scrolled, in lines.
    pub fn detail_scroll(&self) -> usize {
        self.detail_scroll
    }

    /// Record what the frame just drawn measured the detail region's scrollable
    /// extent to be, or that it measured nothing.
    ///
    /// The offset is pulled back with it, so a terminal that shrank between
    /// frames cannot leave the state pointing past the end of the content.
    /// This is the one place geometry reaches the application state, and it is
    /// not a reducer transition: `update` stays free of anything the terminal
    /// knows and the renderer measures.
    ///
    /// `None` — a frame that did not draw the thing — is itself recorded. An
    /// extent kept from an earlier frame is a measurement of content that is
    /// not on screen now, and a terminal too small to draw the install dialog
    /// at all would otherwise leave a stale zero standing for "the reader has
    /// seen the whole plan".
    pub fn note_detail_max_scroll(&mut self, max_scroll: Option<usize>) {
        self.detail_measured = max_scroll.is_some();
        if let Some(max_scroll) = max_scroll {
            self.detail_max_scroll = max_scroll;
            self.detail_scroll = self.detail_scroll.min(max_scroll);
        }
    }

    pub fn inventory_filter_active(&self) -> bool {
        self.inventory_filter_active
    }

    /// Whether the filter box can be opened from where the user is standing.
    pub fn can_filter_inventory(&self) -> bool {
        self.view == View::Inventory
            && self.inventory_pane == InventoryPane::Skills
            && !self.inventory.rows().is_empty()
    }

    /// The rows the current filter admits, in snapshot order.
    pub fn filtered_rows(&self) -> Vec<&InventoryRow> {
        let rows = self.inventory.rows();
        self.filtered_installations
            .iter()
            .filter_map(|index| rows.get(*index))
            .collect()
    }

    /// How many rows the filter admits, without materialising them.
    ///
    /// The key-hint bar asks this on every frame, so it must not allocate.
    pub fn filtered_installation_count(&self) -> usize {
        self.filtered_installations.len()
    }

    pub fn selected_installation(&self) -> Option<&InventoryRow> {
        self.filtered_installations
            .get(self.focused_installation)
            .and_then(|index| self.inventory.rows().get(*index))
    }

    pub fn selected_source(&self) -> Option<&RegisteredSource> {
        self.sources.get(self.focused_source)
    }

    pub fn preview_source(&self, path: &Path) -> Result<SourcePreview> {
        let resolved = match path.strip_prefix("~") {
            Ok(relative) => self.environment.home_dir.join(relative),
            Err(_) => path.to_path_buf(),
        };
        preview_local_source(&resolved)
    }

    pub fn confirm_source(&mut self, preview: SourcePreview) -> Result<()> {
        let preview = revalidate_source_preview(&preview)?;
        match self.register_and_refresh_source(&preview) {
            Ok(()) => {}
            Err(RegistrationFailure::Request(error)) => return Err(error),
            Err(RegistrationFailure::Metadata(failure)) => {
                self.degrade(failure.clone());
                return Err(Error::MetadataUnavailable(failure));
            }
        }
        self.focus_registered_source(preview.inspected().git_top_level());
        self.rescan_installations();
        Ok(())
    }

    fn register_and_refresh_source(
        &mut self,
        preview: &SourcePreview,
    ) -> std::result::Result<(), RegistrationFailure> {
        let (result, committed) = match &mut self.metadata {
            Metadata::Ready(store) => {
                let database_path = store.database_path().to_path_buf();
                match store.register_source(preview) {
                    Ok(()) => (
                        store.registered_sources().map_err(|error| {
                            MetadataFailure::new(database_path, error.to_string())
                        }),
                        true,
                    ),
                    // A refusal of the request is not a failure of the store.
                    // Nothing was committed, the metadata is exactly as usable
                    // as it was, and the flow keeps the error so another path
                    // can be offered.
                    Err(error) if is_source_request_error(&error) => {
                        return Err(RegistrationFailure::Request(error));
                    }
                    Err(error) => (
                        Err(MetadataFailure::new(database_path, error.to_string())),
                        false,
                    ),
                }
            }
            Metadata::Unavailable(failure) => {
                return Err(RegistrationFailure::Metadata(failure.clone()));
            }
        };
        match result {
            Ok(sources) => {
                self.sources = sources;
                Ok(())
            }
            Err(failure) => {
                if committed {
                    self.registry_availability = RegistryAvailability::Unavailable;
                }
                Err(RegistrationFailure::Metadata(failure))
            }
        }
    }

    /// Enter the session's fail-closed read-only state while retaining data
    /// that was read successfully before the failure.
    ///
    /// Reached only from a failure of the metadata store itself. Degrading is
    /// irreversible for the session, so a recoverable refusal of one request
    /// must never take this route.
    fn degrade(&mut self, failure: MetadataFailure) {
        self.set_degraded(failure);
        self.rescan_installations();
    }

    fn set_degraded(&mut self, failure: MetadataFailure) {
        if matches!(self.metadata, Metadata::Ready(_)) {
            self.metadata = Metadata::Unavailable(failure);
        }
        self.view = View::Inventory;
        self.clear_pending_source_state();
        self.pending_operation = None;
        self.pending_repair = None;
        self.pending_update = None;
    }

    pub fn update(&mut self, action: Action) -> UpdateResult {
        if self.help_context.is_some() {
            return match action {
                Action::CloseHelp => {
                    self.help_context = None;
                    UpdateResult::continuing(Vec::new())
                }
                Action::Quit => self.quit_result(),
                _ => UpdateResult::continuing(Vec::new()),
            };
        }

        // A preview is a question about writes that have not happened yet, so
        // it owns the keyboard until it is answered: nothing may navigate out
        // from under it, and nothing but a confirmation may confirm it.
        if self.pending_operation.is_some() {
            return match action {
                Action::Quit => self.quit_result(),
                Action::ConfirmOperation => UpdateResult::continuing(self.confirm_operation()),
                Action::DismissOperation => {
                    self.pending_operation = None;
                    self.reset_detail_scroll();
                    UpdateResult::continuing(Vec::new())
                }
                // A dialog taller than the terminal is still one the reader has
                // to be able to read all of before agreeing to it.
                Action::ScrollDetail(delta) => {
                    self.scroll_detail(delta);
                    UpdateResult::continuing(Vec::new())
                }
                _ => UpdateResult::continuing(Vec::new()),
            };
        }

        if self.pending_repair.is_some() {
            return match action {
                Action::Quit => self.quit_result(),
                Action::ConfirmRepair => UpdateResult::continuing(self.confirm_repair()),
                Action::DismissRepair => {
                    self.pending_repair = None;
                    self.reset_detail_scroll();
                    UpdateResult::continuing(Vec::new())
                }
                Action::ScrollDetail(delta) => {
                    self.scroll_detail(delta);
                    UpdateResult::continuing(Vec::new())
                }
                _ => UpdateResult::continuing(Vec::new()),
            };
        }

        if self.pending_update.is_some() {
            return match action {
                Action::Quit => self.quit_result(),
                Action::ConfirmRepositoryUpdate => {
                    let effects = matches!(self.pending_update, Some(RepositoryUpdatePrompt::Preview(ref plan)) if !plan.is_blocked() && self.update_preview_fully_seen)
                        .then_some(Effect::ApplyRepositoryUpdate).into_iter().collect();
                    UpdateResult::continuing(effects)
                }
                Action::DismissRepositoryUpdate => {
                    self.pending_update = None;
                    self.reset_detail_scroll();
                    UpdateResult::continuing(Vec::new())
                }
                Action::ScrollDetail(delta) => {
                    self.scroll_detail(delta);
                    UpdateResult::continuing(Vec::new())
                }
                _ => UpdateResult::continuing(Vec::new()),
            };
        }

        // The filter bar owns the keyboard while it is open, so a stray action
        // cannot navigate out from under a half-typed query. Quitting is the
        // one command no context may swallow.
        if self.inventory_filter_active {
            return match action {
                Action::Quit => self.quit_result(),
                _ => {
                    self.filter_input(action);
                    UpdateResult::continuing(Vec::new())
                }
            };
        }

        let effects = match action {
            Action::Continue => self.advance_setup(),
            Action::Back => return self.back(),
            Action::MoveSelection(delta) => {
                self.move_selection(delta);
                Vec::new()
            }
            Action::ToggleSelection => {
                self.toggle_selection();
                Vec::new()
            }
            Action::OpenHelp => {
                if !self.source_path_input_active
                    && !self.inventory_filter_active
                    && self.pending_source.is_none()
                {
                    self.help_context = Some(self.view);
                }
                Vec::new()
            }
            Action::CloseHelp => {
                self.help_context = None;
                Vec::new()
            }
            Action::OpenSettings => {
                self.open_settings();
                Vec::new()
            }
            Action::OpenInventory => {
                if matches!(self.view, View::Sources | View::Updates | View::Doctor) {
                    self.enter_inventory()
                } else {
                    Vec::new()
                }
            }
            Action::OpenSources => {
                if matches!(self.view, View::Inventory | View::Updates | View::Doctor) {
                    self.view = View::Sources;
                    self.sources_pane = SourcesPane::Repositories;
                }
                Vec::new()
            }
            Action::OpenUpdates => {
                if matches!(self.view, View::Inventory | View::Sources | View::Doctor) {
                    self.view = View::Updates;
                    self.updates_pane = UpdatesPane::Candidates;
                    self.reset_detail_scroll();
                }
                Vec::new()
            }
            Action::OpenDoctor => {
                if matches!(self.view, View::Inventory | View::Sources | View::Updates) {
                    self.enter_doctor()
                } else {
                    Vec::new()
                }
            }
            Action::MoveDoctorPane(delta) => {
                if self.view == View::Doctor {
                    let index = match self.doctor_pane {
                        DoctorPane::Findings => 0,
                        DoctorPane::Details => 1,
                    };
                    self.doctor_pane = match wrapped_index(index, delta, 2) {
                        0 => DoctorPane::Findings,
                        _ => DoctorPane::Details,
                    };
                }
                Vec::new()
            }
            Action::MoveUpdatesPane(delta) => {
                if self.view == View::Updates {
                    let index = usize::from(self.updates_pane == UpdatesPane::Details);
                    self.updates_pane = if wrapped_index(index, delta, 2) == 0 {
                        UpdatesPane::Candidates
                    } else {
                        UpdatesPane::Details
                    };
                }
                Vec::new()
            }
            Action::AdvanceUpdatesPane => {
                if self.view != View::Updates
                    || self.sources.is_empty()
                    || self.update_check_in_flight()
                {
                    Vec::new()
                } else if self.updates_pane == UpdatesPane::Candidates {
                    self.updates_pane = UpdatesPane::Details;
                    Vec::new()
                } else if self.selected_update_source().is_some_and(|source| {
                    self.update_check_for(source.id()).is_some_and(|check| {
                        !check.superseded_by(source)
                            && check.verdict == RepositoryUpdateVerdict::Available
                    })
                }) {
                    vec![Effect::PlanRepositoryUpdate]
                } else {
                    Vec::new()
                }
            }
            Action::MoveUpdatesSelection(delta) => {
                if self.view == View::Updates && self.updates_pane == UpdatesPane::Candidates {
                    self.focused_update =
                        wrapped_index(self.focused_update, delta, self.sources.len());
                    self.reset_detail_scroll();
                }
                Vec::new()
            }
            Action::BeginUpdateCheck => {
                if self.view == View::Updates
                    && !self.sources.is_empty()
                    && !self.update_check_in_flight()
                {
                    vec![Effect::CheckUpdates]
                } else {
                    Vec::new()
                }
            }
            Action::CancelUpdateCheck => {
                if self.view == View::Updates && self.update_check_in_flight() {
                    vec![Effect::CancelUpdateCheck]
                } else {
                    Vec::new()
                }
            }
            Action::BeginRepositoryUpdate => {
                if self.view == View::Updates
                    && self.updates_pane == UpdatesPane::Details
                    && !self.update_check_in_flight()
                {
                    vec![Effect::PlanRepositoryUpdate]
                } else {
                    Vec::new()
                }
            }
            Action::ConfirmRepositoryUpdate | Action::DismissRepositoryUpdate => Vec::new(),
            Action::AdvanceDoctorPane => {
                if self.view == View::Doctor && self.selected_finding().is_some() {
                    self.doctor_pane = DoctorPane::Details;
                }
                Vec::new()
            }
            Action::MoveDoctorSelection(delta) => {
                if self.view == View::Doctor && self.doctor_pane == DoctorPane::Findings {
                    self.focused_finding =
                        wrapped_index(self.focused_finding, delta, self.finding_count());
                    // The window belonged to the finding that was selected.
                    self.reset_detail_scroll();
                }
                Vec::new()
            }
            Action::BeginAddSource => {
                if self.can_add_source()
                    && (self.view == View::Sources
                        || self.view == View::Setup(SetupStep::DiscoverSources))
                {
                    self.source_path.clear();
                    self.source_error = None;
                    self.pending_source = None;
                    self.source_path_input_active = true;
                }
                Vec::new()
            }
            Action::AppendSourcePath(character) => {
                if self.source_path_input_active && !character.is_control() {
                    self.source_path.push(character);
                }
                Vec::new()
            }
            Action::DeleteSourcePathCharacter => {
                if self.source_path_input_active {
                    self.source_path.pop();
                }
                Vec::new()
            }
            Action::SubmitSourcePath => self.submit_source_path(),
            Action::CancelSourceFlow => {
                if self.view == View::Setup(SetupStep::ConfirmCatalogs) {
                    self.view = View::Setup(SetupStep::DiscoverSources);
                }
                self.clear_pending_source_state();
                Vec::new()
            }
            Action::MoveCatalogSelection(delta) => {
                if let Some(preview) = &self.pending_source {
                    self.focused_catalog =
                        wrapped_index(self.focused_catalog, delta, preview.catalogs().len());
                }
                Vec::new()
            }
            Action::ToggleCatalogIncluded => {
                if let Some(catalog) = self
                    .pending_source
                    .as_mut()
                    .and_then(|preview| preview.catalog_mut(self.focused_catalog))
                {
                    catalog.toggle_included();
                }
                Vec::new()
            }
            Action::ToggleCatalogClassification => {
                if let Some(catalog) = self
                    .pending_source
                    .as_mut()
                    .and_then(|preview| preview.catalog_mut(self.focused_catalog))
                {
                    catalog.toggle_classification();
                }
                Vec::new()
            }
            Action::ToggleCatalogCompatibility(agent) => {
                if let Some(catalog) = self
                    .pending_source
                    .as_mut()
                    .and_then(|preview| preview.catalog_mut(self.focused_catalog))
                {
                    catalog.toggle_compatibility(agent);
                }
                Vec::new()
            }
            Action::ConfirmPendingSource if self.can_add_source() => self.register_pending_source(),
            Action::ConfirmPendingSource => Vec::new(),
            Action::MoveSourcesPane(delta) => {
                if self.view == View::Sources {
                    let index = match self.sources_pane {
                        SourcesPane::Repositories => 0,
                        SourcesPane::Variants => 1,
                        SourcesPane::Details => 2,
                    };
                    self.sources_pane = match wrapped_index(index, delta, 3) {
                        0 => SourcesPane::Repositories,
                        1 => SourcesPane::Variants,
                        _ => SourcesPane::Details,
                    };
                }
                Vec::new()
            }
            Action::AdvanceSourcesPane => {
                if self.view == View::Sources {
                    self.sources_pane = match self.sources_pane {
                        SourcesPane::Repositories if self.selected_source().is_some() => {
                            SourcesPane::Variants
                        }
                        SourcesPane::Variants if self.selected_source().is_some() => {
                            SourcesPane::Details
                        }
                        current => current,
                    };
                }
                Vec::new()
            }
            Action::MoveSourcesSelection(delta) => {
                self.move_sources_selection(delta);
                Vec::new()
            }
            Action::MoveInventoryPane(delta) => {
                if self.view == View::Inventory {
                    let index = match self.inventory_pane {
                        InventoryPane::Skills => 0,
                        InventoryPane::Details => 1,
                    };
                    self.inventory_pane = match wrapped_index(index, delta, 2) {
                        0 => InventoryPane::Skills,
                        _ => InventoryPane::Details,
                    };
                }
                Vec::new()
            }
            Action::AdvanceInventoryPane => {
                if self.view == View::Inventory && self.selected_installation().is_some() {
                    self.inventory_pane = InventoryPane::Details;
                }
                Vec::new()
            }
            Action::MoveInventorySelection(delta) => {
                self.move_installation_selection(delta);
                Vec::new()
            }
            Action::ScrollDetail(delta) => {
                self.scroll_detail(delta);
                Vec::new()
            }
            Action::BeginInventoryFilter => {
                // The query box is drawn above the table, and a compact
                // terminal showing the detail region has no table to draw it
                // above. Opening it there would take every printable key for a
                // field the user cannot see. Filtering an empty inventory is
                // refused for the same reason: nothing would narrow.
                if self.can_filter_inventory() {
                    self.inventory_filter_active = true;
                }
                Vec::new()
            }
            Action::AppendInventoryFilter(_)
            | Action::DeleteInventoryFilterCharacter
            | Action::SubmitInventoryFilter => Vec::new(),
            Action::BeginInstall => {
                if self.pending_repair.is_none() && self.can_install_selection() {
                    vec![Effect::PlanInstall]
                } else {
                    Vec::new()
                }
            }
            Action::BeginUninstall => {
                if self.can_uninstall_selection() {
                    vec![Effect::PlanUninstall]
                } else {
                    Vec::new()
                }
            }
            Action::BeginForgetSource => {
                if self.can_forget_source() {
                    vec![Effect::PlanForgetSource]
                } else {
                    Vec::new()
                }
            }
            // Reachable only with no prompt open, where there is nothing to
            // confirm and nothing to dismiss.
            Action::ConfirmOperation | Action::DismissOperation => Vec::new(),
            Action::BeginRepair => {
                if self.pending_operation.is_none() && self.can_repair_selection() {
                    vec![Effect::PlanRepair]
                } else {
                    Vec::new()
                }
            }
            Action::ConfirmRepair | Action::DismissRepair => Vec::new(),
            Action::RerunSetup if self.can_rerun_setup() => self.rerun_setup(),
            Action::RerunSetup => Vec::new(),
            Action::Quit => return self.quit_result(),
        };
        UpdateResult::continuing(effects)
    }

    pub fn perform_effects(&mut self, effects: &[Effect]) -> Result<()> {
        for effect in effects {
            match effect {
                Effect::PersistSetup { agent_selections } => {
                    // These are the selections the user just made. They remain
                    // the scan scope even if persisting them is the operation
                    // that forces this session into degraded mode.
                    self.scan_scope_known = true;
                    let result = match &mut self.metadata {
                        Metadata::Ready(store) => {
                            store.complete_setup(*agent_selections).map_err(|error| {
                                MetadataFailure::new(
                                    store.database_path().to_path_buf(),
                                    error.to_string(),
                                )
                            })
                        }
                        Metadata::Unavailable(failure) => Err(failure.clone()),
                    };
                    if let Err(failure) = result {
                        self.degrade(failure);
                    }
                }
                Effect::ResetSetup => {
                    let result = match &self.metadata {
                        Metadata::Ready(store) => {
                            store.set_setup_complete(false).map_err(|error| {
                                MetadataFailure::new(
                                    store.database_path().to_path_buf(),
                                    error.to_string(),
                                )
                            })
                        }
                        Metadata::Unavailable(failure) => Err(failure.clone()),
                    };
                    if let Err(failure) = result {
                        self.degrade(failure);
                    }
                }
                Effect::RedetectAgents { agent_selections } => {
                    let mut detections = detect_agents(&self.environment);
                    for (detection, selected) in detections.iter_mut().zip(agent_selections) {
                        detection.set_selected(*selected);
                    }
                    self.agents = detections;
                }
                Effect::InspectSource { path } => match self.preview_source(path) {
                    Ok(preview) if preview.catalogs().is_empty() => {
                        self.source_error = Some(
                            "No supported skill catalog roots were found in this checkout."
                                .to_owned(),
                        );
                        self.source_path_input_active = true;
                    }
                    Ok(preview) => {
                        self.pending_source = Some(preview);
                        self.source_path_input_active = false;
                        self.source_error = None;
                        self.focused_catalog = 0;
                        if self.view == View::Setup(SetupStep::DiscoverSources) {
                            self.view = View::Setup(SetupStep::ConfirmCatalogs);
                        }
                    }
                    Err(error) => {
                        self.source_error = Some(error.to_string());
                        self.source_path_input_active = true;
                    }
                },
                Effect::RegisterSource { preview } => {
                    let preview = match revalidate_source_preview(preview) {
                        Ok(preview) => preview,
                        Err(error) => {
                            self.source_error = Some(error.to_string());
                            continue;
                        }
                    };
                    match self.register_and_refresh_source(&preview) {
                        Ok(()) => {}
                        // Reported where a failed revalidation is reported, and
                        // for the same reason: the checkout was refused, not
                        // the store, so the flow stays open for another path.
                        Err(RegistrationFailure::Request(error)) => {
                            self.source_error = Some(error.to_string());
                            continue;
                        }
                        Err(RegistrationFailure::Metadata(failure)) => {
                            self.degrade(failure);
                            continue;
                        }
                    }
                    self.focus_registered_source(preview.inspected().git_top_level());
                    self.pending_source = None;
                    self.source_path.clear();
                    self.source_path_input_active = false;
                    self.source_error = None;
                    self.focused_catalog = 0;
                    if self.view == View::Setup(SetupStep::ConfirmCatalogs) {
                        self.view = View::Setup(SetupStep::ScanInstallations);
                    }
                    // A new source can turn unmanaged content into a resolved
                    // installation, so the inventory is restated immediately.
                    self.rescan_installations();
                }
                Effect::ScanInstallations => self.rescan_installations(),
                Effect::PlanInstall => {
                    match self.build_install_preview() {
                        Ok(prompt) => {
                            self.pending_operation = Some(OperationPrompt::Install(prompt));
                        }
                        Err(failure) => self.degrade(failure),
                    }
                    // The window belongs to the content under it, and this is
                    // new content.
                    self.reset_detail_scroll();
                }
                Effect::ApplyInstall => self.apply_pending_install(),
                Effect::PlanUninstall => {
                    self.pending_operation =
                        Some(OperationPrompt::Uninstall(self.build_uninstall_preview()));
                    self.reset_detail_scroll();
                }
                Effect::ApplyUninstall => self.apply_pending_uninstall(),
                Effect::PlanForgetSource => {
                    self.pending_operation =
                        Some(OperationPrompt::Forget(self.build_forget_preview()));
                    self.reset_detail_scroll();
                }
                Effect::ApplyForgetSource => self.apply_pending_forget(),
                Effect::PlanRepair => {
                    self.pending_repair = Some(self.build_repair_preview());
                    self.reset_detail_scroll();
                }
                Effect::ApplyRepair => self.apply_pending_repair(),
                Effect::CheckUpdates => self.start_update_check(),
                Effect::CancelUpdateCheck => self.cancel_update_check(),
                Effect::RecordUpdateChecks(checks) => self.persist_update_checks(checks),
                Effect::FinishUpdateCheck => self.finish_update_check(),
                Effect::PlanRepositoryUpdate => {
                    // The plan names which installed skills the fast-forward
                    // touches, and derives them from the inventory. A link into
                    // this source made while the application stayed open is
                    // absent from the last scan, so the confirmation would omit
                    // an installation the update changes and the post-apply
                    // rescan would then report that pre-existing link as having
                    // appeared undisclosed. The scan is filesystem work, so it
                    // happens here at the effect boundary rather than in the
                    // reducer.
                    self.rescan_installations();
                    self.pending_update = Some(self.build_repository_update_preview());
                    self.reset_detail_scroll();
                }
                Effect::ApplyRepositoryUpdate => self.apply_pending_repository_update(),
            }
        }
        Ok(())
    }

    /// Reserve `count` consecutive generations across every Skilled process,
    /// and return the first.
    ///
    /// The store is the only thing that can order this process against another,
    /// so a reservation it refuses is a failure rather than something to route
    /// around: a process-local value handed out instead is exactly the
    /// collision this exists to prevent, and the conditional upsert would then
    /// drop a result while reporting that it was stored. Callers either refuse
    /// the work or carry the failure into what they report.
    fn reserve_generations(&mut self, count: usize) -> std::result::Result<i64, String> {
        let span = i64::try_from(count.max(1)).unwrap_or(i64::MAX) - 1;
        // `now()` rather than the bare clock: it carries this process's own
        // floor, which a clock that moved backwards mid-session would fall
        // below. The store contributes what every process has recorded; this
        // contributes what this one has handed out.
        let first = self
            .store_mut()
            .and_then(|store| store.reserve_update_check_generations(now(), count))
            .map_err(|error| format!("update check generations could not be reserved: {error}"))?;
        note_generation(first.saturating_add(span));
        Ok(first)
    }

    fn start_update_check(&mut self) {
        if self.update_check_run.is_some() {
            return;
        }
        self.update_check_error = None;
        let sources = self.sources.clone();
        // The roots as they stand now, for the same reason
        // `Effect::PlanRepositoryUpdate` reads them again: a check decides the
        // findings the preview decides, and both of them turn on which
        // installations resolve into this source. Handing the worker the last
        // completed scan would let a link made while the application stayed
        // open be missing from the cached verdict and present in the preview —
        // Updates offering an update the preview then refuses, which is the
        // contradiction caching these findings exists to close. The scan is
        // filesystem work and this is the effect boundary, where it belongs.
        self.rescan_installations();
        // Carried into the worker rather than read there: the worker has no
        // application state, and reading agent roots is not what a cancellable
        // repository check is for.
        let inventory = self.inventory.clone();
        // The whole run's generations are taken before any of it starts, so
        // every check this run records is ordered ahead of anything another
        // process reserved and behind anything it reserves next. The worker
        // thread has no store of its own to allocate from.
        //
        // A refusal ends the run here. The store that cannot hand out an
        // ordered generation is the store that would have to accept the
        // results, so reaching the network first would spend it on a check
        // nothing could safely record.
        let first_generation = match self.reserve_generations(sources.len()) {
            Ok(first) => first,
            Err(error) => {
                self.update_check_error = Some(error);
                return;
            }
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let terminal_state = Arc::new(AtomicU8::new(UPDATE_CHECK_RUNNING));
        let child = Arc::new(Mutex::new(None));
        let (sender, receiver) = mpsc::channel();
        let worker_cancelled = Arc::clone(&cancelled);
        let worker_terminal_state = Arc::clone(&terminal_state);
        let panic_terminal_state = Arc::clone(&terminal_state);
        let worker_child = Arc::clone(&child);
        let panic_sender = sender.clone();
        let handle = std::thread::spawn(move || {
            let result = crate::terminal::catch_update_worker_panic(|| {
                let mut checks = Vec::with_capacity(sources.len());
                let total = sources.len();
                let _ = sender.send(UpdateCheckMessage::Progress {
                    completed: 0,
                    total,
                });
                for (index, source) in sources.iter().enumerate() {
                    if worker_cancelled.load(Ordering::Acquire) {
                        let _ = sender.send(UpdateCheckMessage::Cancelled);
                        return;
                    }
                    let Some(probe) = probe_repository_update_cancellable(
                        source,
                        &worker_cancelled,
                        &worker_child,
                    ) else {
                        let _ = sender.send(UpdateCheckMessage::Cancelled);
                        return;
                    };
                    let Some(check) = cached_update_check(
                        source,
                        &probe,
                        &inventory,
                        first_generation.saturating_add(i64::try_from(index).unwrap_or(i64::MAX)),
                        &worker_cancelled,
                    ) else {
                        let _ = sender.send(UpdateCheckMessage::Cancelled);
                        return;
                    };
                    checks.push(check);
                    if worker_cancelled.load(Ordering::Acquire) {
                        let _ = sender.send(UpdateCheckMessage::Cancelled);
                        return;
                    }
                    let _ = sender.send(UpdateCheckMessage::Progress {
                        completed: index + 1,
                        total,
                    });
                }
                if worker_cancelled.load(Ordering::Acquire) {
                    let _ = sender.send(UpdateCheckMessage::Cancelled);
                } else if worker_terminal_state
                    .compare_exchange(
                        UPDATE_CHECK_RUNNING,
                        UPDATE_CHECK_FINISHED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    let _ = sender.send(UpdateCheckMessage::Finished(checks));
                } else {
                    let _ = sender.send(UpdateCheckMessage::Cancelled);
                }
            });
            if result.is_err() {
                publish_update_worker_failure(&panic_terminal_state, &panic_sender);
            }
        });
        self.update_check_run = Some(UpdateCheckRun {
            receiver,
            handle,
            cancelled,
            terminal_state,
            child,
        });
        self.update_check_progress = Some((0, self.sources.len()));
    }

    fn cancel_update_check(&mut self) {
        let mut finished = None;
        let mut disconnected = false;
        if let Some(run) = self.update_check_run.as_mut() {
            loop {
                match run.receiver.try_recv() {
                    Ok(UpdateCheckMessage::Progress { completed, total }) => {
                        self.update_check_progress = Some((completed, total));
                    }
                    Ok(UpdateCheckMessage::Finished(checks)) => {
                        finished = Some(checks);
                        break;
                    }
                    Ok(UpdateCheckMessage::Failed(error)) => {
                        self.update_check_error = Some(error);
                        disconnected = true;
                        break;
                    }
                    Ok(UpdateCheckMessage::Cancelled) => {
                        disconnected = true;
                        break;
                    }
                    Err(TryRecvError::Disconnected) => {
                        if !run.cancelled.load(Ordering::Acquire) {
                            self.update_check_error =
                                Some("update check ended before completing".to_owned());
                        }
                        disconnected = true;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                }
            }
        }
        if let Some(checks) = finished {
            if let Some(run) = self.update_check_run.take() {
                self.retire_update_worker(run.handle);
            }
            self.update_check_progress = None;
            self.persist_update_checks(&checks);
            return;
        }
        if disconnected {
            if let Some(run) = self.update_check_run.take() {
                self.retire_update_worker(run.handle);
            }
            self.update_check_progress = None;
            return;
        }
        if let Some(run) = self.update_check_run.take() {
            if run
                .terminal_state
                .compare_exchange(
                    UPDATE_CHECK_RUNNING,
                    UPDATE_CHECK_CANCELLED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                run.cancelled.store(true, Ordering::Release);
                if let Some(mut child) = run
                    .child
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .take()
                {
                    crate::git::terminate_child(&mut child);
                }
            }
            // Non-fetch inspection children are not yet published through the
            // cancellable child slot. Keep ownership of the run until its
            // thread actually exits so another check cannot overlap it.
            self.update_check_run = Some(run);
        }
    }

    fn persist_update_checks(&mut self, checks: &[CachedUpdateCheck]) {
        if let Err(error) = self
            .store_mut()
            .and_then(|store| store.record_update_checks(checks))
        {
            self.update_check_error = Some(format!("update checks could not be saved: {error}"));
            return;
        }
        let sources = match self.store().and_then(Store::registered_sources) {
            Ok(sources) => sources,
            Err(error) => {
                self.update_check_error =
                    Some(format!("source state could not be refreshed: {error}"));
                return;
            }
        };
        let cached = match self.store().and_then(Store::update_checks) {
            Ok(cached) => cached,
            Err(error) => {
                self.update_check_error = Some(format!(
                    "saved update checks could not be read back: {error}"
                ));
                return;
            }
        };
        self.sources = sources;
        self.update_checks = cached;
        self.update_check_error = None;
    }

    fn finish_update_check(&mut self) {
        if let Some(run) = self.update_check_run.take() {
            self.retire_update_worker(run.handle);
        }
        self.update_check_progress = None;
    }

    fn retire_update_worker(&mut self, handle: JoinHandle<()>) {
        if handle.is_finished() {
            let _ = handle.join();
        } else {
            self.retired_update_workers.push(handle);
        }
    }

    fn quit_result(&self) -> UpdateResult {
        UpdateResult::quit_with(
            self.update_check_in_flight()
                .then_some(Effect::CancelUpdateCheck)
                .into_iter()
                .collect(),
        )
    }

    fn build_repository_update_preview(&self) -> RepositoryUpdatePrompt {
        let Some(source) = self.selected_update_source() else {
            return RepositoryUpdatePrompt::Failed("no registered source is selected".into());
        };
        let Some(check) = self.update_check_for(source.id()) else {
            return RepositoryUpdatePrompt::Failed(
                "check this source before previewing an update".into(),
            );
        };
        if check.superseded_by(source) || check.verdict != RepositoryUpdateVerdict::Available {
            return RepositoryUpdatePrompt::Failed(
                "the cached check does not describe an available update for the current checkout"
                    .into(),
            );
        }
        let (Some(upstream_ref), Some(upstream_revision)) =
            (&check.upstream_ref, &check.upstream_revision)
        else {
            return RepositoryUpdatePrompt::Failed(
                "the cached check does not identify an upstream object".into(),
            );
        };
        let probe = probe_repository_update_against(source, upstream_ref, upstream_revision);
        match plan_repository_update(source, &probe, &self.inventory) {
            Ok(plan) => RepositoryUpdatePrompt::Preview(plan),
            Err(error) => RepositoryUpdatePrompt::Failed(error.to_string()),
        }
    }

    fn apply_pending_repository_update(&mut self) {
        let Some(RepositoryUpdatePrompt::Preview(plan)) = self.pending_update.take() else {
            return;
        };
        let before_inventory = self.inventory.clone();
        // Read before the write, while `update_checks` still holds the check
        // this plan was built from and nothing has been reloaded over it.
        let planned_at = self.plan_check_generation(plan.source_id());
        let (apply_result, write_attempted) = apply_repository_update_attempt(&plan);
        let apply_error = apply_result.err().map(|error| error.to_string());
        // Asked before the first read that follows the write, because every
        // one of them reads objects and a disclosed hook has already had its
        // turn. Refreshing the registered source runs `status` and
        // `cat-file`; the rescan resolves links into the checkout; and
        // verification reads HEAD. None of them may run against a repository
        // that has become able to fetch on their behalf.
        self.sources = match self.store().and_then(Store::registered_sources) {
            Ok(sources) => sources,
            Err(error) => {
                self.pending_update = Some(RepositoryUpdatePrompt::StateUnavailable {
                    apply_error,
                    write_attempted,
                    refresh_error: error.to_string(),
                });
                self.reset_detail_scroll();
                return;
            }
        };
        self.rescan_installations();
        let verification = verify_repository_update_attempt(
            &plan,
            &before_inventory,
            &self.inventory,
            write_attempted,
        );
        // A generation of its own for every answer but the verified one. A
        // failure is a later observation than the check it followed, and
        // reusing that check's generation would leave it losing the
        // conditional upsert to anything another process recorded while the
        // preview was open — a store that reports success and keeps the
        // pre-update verdict, taking a verification failure with it. The
        // verified answer is the opposite case and takes the plan's own
        // generation; see [`Self::repository_verification_check`].
        //
        // The write has already happened, so a refused reservation cannot end
        // the operation the way it ends a check — but it does end the caching.
        // A process-local value is one another process may already own, and
        // recording under it would let this row displace theirs or theirs
        // displace this one, which is the corruption the reservation exists to
        // prevent. The report still states the verification either way; only
        // the cache goes without, and it says so.
        let persistence_error = match self.reserve_generations(1) {
            Err(error) => Some(error),
            Ok(checked_at) => {
                let check = if write_attempted {
                    apply_error.as_deref().map_or_else(
                        || {
                            self.repository_verification_check(
                                &plan,
                                &verification,
                                planned_at,
                                checked_at,
                            )
                        },
                        |error| {
                            self.repository_apply_failure_check(
                                &plan,
                                error,
                                &verification,
                                checked_at,
                            )
                        },
                    )
                } else {
                    self.superseded_repository_check(plan.source_id(), checked_at)
                };
                self.store()
                    .and_then(|store| store.record_update_check(&check))
                    .and_then(|()| self.store().and_then(Store::update_checks))
                    .map(|checks| self.update_checks = checks)
                    .err()
                    .map(|error| format!("verified update state could not be cached: {error}"))
            }
        };
        self.update_check_error = persistence_error.clone();
        self.pending_update = Some(RepositoryUpdatePrompt::Report {
            plan,
            verification,
            apply_error,
            write_attempted,
            persistence_error,
        });
        self.reset_detail_scroll();
    }

    pub(crate) fn plan_repository_update_for(
        &mut self,
        source_id: i64,
    ) -> std::result::Result<RepositoryUpdatePlan, String> {
        let source = self
            .sources
            .iter()
            .find(|source| source.id() == source_id)
            .cloned()
            .ok_or_else(|| "no registered source has that identifier".to_owned())?;
        // Reserved before the probe, exactly as `start_update_check` does it.
        // The probe fetches, which is the slow part, and a generation taken
        // afterwards would order this check by when its network call happened
        // to return rather than by when it was asked for — letting a check
        // that began earlier, and therefore describes an older upstream,
        // displace one that began later.
        let generation = self.reserve_generations(1)?;
        let probe = probe_repository_update(&source, true);
        // Read after the probe, which fetched and may have taken a while. The
        // plan is about to state which installations the update affects, and
        // the roots it derives that from must be the roots as they are now:
        // this is the same reading `Effect::PlanRepositoryUpdate` takes, and
        // the same reading the post-apply verification will be compared
        // against. Doing it here also gives that comparison two snapshots of
        // equal standing, so a command run before setup completes reports what
        // it verified rather than withholding the answer.
        //
        // Before the check is recorded, not after: the check states the same
        // installation-affecting findings the plan does, and deriving them from
        // an older reading than the plan's would cache a verdict this very
        // command then contradicts.
        self.rescan_installations();
        // A typed command has nothing to cancel it, so the analysis behind the
        // check cannot be interrupted and always has an answer.
        let check = cached_update_check(
            &source,
            &probe,
            &self.inventory,
            generation,
            &AtomicBool::new(false),
        )
        .expect("a flag that is never set cannot cancel this check");
        self.store()
            .and_then(|store| store.record_update_check(&check))
            .map_err(|error| error.to_string())?;
        self.update_checks = self
            .store()
            .and_then(Store::update_checks)
            .map_err(|error| error.to_string())?;
        plan_repository_update(&source, &probe, &self.inventory).map_err(|error| error.to_string())
    }

    /// Apply a plan for `skilled update`, then restate the inventory and check
    /// the plan against it.
    ///
    /// The rescan reads the agent roots whether or not setup has run, exactly
    /// as [`Self::apply_plan`] does for `skilled install`: a command that was
    /// asked to write cannot report a verified result without reading what it
    /// wrote. Withholding the roots until the user has chosen agents belongs to
    /// the first-run screen, which opens onto them unasked; a typed command
    /// does not.
    pub(crate) fn apply_repository_plan(
        &mut self,
        plan: &RepositoryUpdatePlan,
    ) -> RepositoryApplyOutcome {
        let before_inventory = self.inventory.clone();
        let planned_at = self.plan_check_generation(plan.source_id());
        let (apply_result, write_attempted) = apply_repository_update_attempt(plan);
        let apply_error = apply_result.err().map(|error| error.to_string());
        self.sources = match self.store().and_then(Store::registered_sources) {
            Ok(sources) => sources,
            Err(error) => {
                return RepositoryApplyOutcome {
                    verification: None,
                    apply_error,
                    bookkeeping_error: Some(format!(
                        "post-attempt source state could not be refreshed: {error}"
                    )),
                    write_attempted,
                };
            }
        };
        self.rescan_installations();
        let verification = verify_repository_update_attempt(
            plan,
            &before_inventory,
            &self.inventory,
            write_attempted,
        );
        // The same fresh generation the screens take, and the same refusal to
        // cache under one no other process can be held off from reusing.
        let bookkeeping_error = match self.reserve_generations(1) {
            Err(error) => Some(error),
            Ok(checked_at) => {
                let check = if write_attempted {
                    apply_error.as_deref().map_or_else(
                        || {
                            self.repository_verification_check(
                                plan,
                                &verification,
                                planned_at,
                                checked_at,
                            )
                        },
                        |error| {
                            self.repository_apply_failure_check(
                                plan,
                                error,
                                &verification,
                                checked_at,
                            )
                        },
                    )
                } else {
                    self.superseded_repository_check(plan.source_id(), checked_at)
                };
                self.store()
                    .and_then(|store| store.record_update_check(&check))
                    .and_then(|()| self.store().and_then(Store::update_checks))
                    .map(|checks| self.update_checks = checks)
                    .err()
                    .map(|error| format!("post-attempt update state could not be cached: {error}"))
            }
        };
        RepositoryApplyOutcome {
            verification: Some(verification),
            apply_error,
            bookkeeping_error,
            write_attempted,
        }
    }

    /// The generation of the cached check this plan was built from, as this
    /// process knows it. `None` for a caller holding none, which has no
    /// generation to date its answer by and takes a fresh one.
    fn plan_check_generation(&self, source_id: i64) -> Option<i64> {
        self.update_check_for(source_id)
            .map(|check| check.checked_at)
    }

    /// The record an apply leaves behind, dated by what it actually observed.
    ///
    /// A verified result states `UpToDate` for the object the plan named and
    /// re-reads no upstream at all, so the newest reading of the remote behind
    /// it is still the explicit check the plan was built from — `planned_at`,
    /// which it therefore records under, replacing that check's own row. A
    /// freshly reserved generation would instead outrank every check another
    /// Skilled process began while this one was applying, whichever of them
    /// persisted first, and replace a known-available update with a verdict
    /// nothing read: the availability would be hidden until somebody checked
    /// again. Ordering by what was observed rather than by when the row was
    /// written settles both interleavings at once, because a check that
    /// reserved after this plan's carries a later generation whenever it
    /// lands.
    ///
    /// That is preferred to re-probing the upstream after the write: it adds
    /// no repository reads to a path the apply guard's proofs no longer cover,
    /// and leaves the user's own explicit check as the only thing that decides
    /// availability, which is what "cached update findings exist only after an
    /// explicit check" already asks of every other surface.
    ///
    /// A failure or an incomplete result takes the fresh generation and
    /// outranks everything, as it did before: it is the only record that a
    /// write went unverified, and Doctor keeps it for the same reason it
    /// survives a changed `HEAD` — it is an observation of the state that
    /// would otherwise supersede it.
    fn repository_verification_check(
        &self,
        plan: &RepositoryUpdatePlan,
        verification: &crate::updates::RepositoryVerifyReport,
        planned_at: Option<i64>,
        checked_at: i64,
    ) -> CachedUpdateCheck {
        let (verdict, detail, generation) = if !verification.is_verified() {
            (
                RepositoryUpdateVerdict::Blocked,
                format!(
                    "update.verification_failed|{}",
                    verification.failures().join("; ")
                ),
                checked_at,
            )
        } else if !verification.is_complete() {
            (
                RepositoryUpdateVerdict::Blocked,
                format!(
                    "update.verification_incomplete|{}",
                    verification.withheld().join("; ")
                ),
                checked_at,
            )
        } else {
            (
                RepositoryUpdateVerdict::UpToDate,
                String::new(),
                planned_at.unwrap_or(checked_at),
            )
        };
        self.repository_result_check(plan, generation, verdict, detail)
    }

    fn repository_apply_failure_check(
        &self,
        plan: &RepositoryUpdatePlan,
        error: &str,
        verification: &crate::updates::RepositoryVerifyReport,
        checked_at: i64,
    ) -> CachedUpdateCheck {
        let mut findings = vec![Finding::new(
            "update.apply_failed",
            FindingSeverity::Critical,
            error.to_owned(),
        )];
        if !verification.is_verified() {
            findings.push(Finding::new(
                "update.verification_failed",
                FindingSeverity::Critical,
                verification.failures().join("; "),
            ));
        } else if !verification.is_complete() {
            findings.push(Finding::new(
                "update.verification_incomplete",
                FindingSeverity::Warning,
                verification.withheld().join("; "),
            ));
        }
        self.repository_result_check(
            plan,
            checked_at,
            RepositoryUpdateVerdict::Blocked,
            encode_findings(&findings),
        )
    }

    fn repository_result_check(
        &self,
        plan: &RepositoryUpdatePlan,
        checked_at: i64,
        verdict: RepositoryUpdateVerdict,
        detail: String,
    ) -> CachedUpdateCheck {
        let source = self
            .sources
            .iter()
            .find(|source| source.id() == plan.source_id());
        let dirty = source.and_then(RegisteredSource::dirty);
        CachedUpdateCheck {
            source_id: plan.source_id(),
            checked_at,
            local_revision: source
                .map(RegisteredSource::head)
                .unwrap_or(plan.target_revision())
                .to_owned(),
            // What the refreshed source reports, not what the plan was probed
            // on: a hook can leave HEAD detached or on another branch, and a
            // record that still named the planned reference would be superseded
            // by the very state it is reporting on — taking a verification
            // failure out of Doctor with it. The plan's reference answers only
            // for a source that can no longer be read.
            local_reference: source.map_or_else(
                || Some(plan.current_reference().to_owned()),
                |source| source.branch().map(|branch| format!("refs/heads/{branch}")),
            ),
            upstream_ref: Some(plan.upstream_ref().into()),
            upstream_revision: Some(plan.target_revision().into()),
            merge_base: Some(plan.target_revision().into()),
            ahead: 0,
            behind: 0,
            dirty: dirty.unwrap_or(false),
            dirty_known: dirty.is_some(),
            verdict,
            detail,
        }
    }

    fn superseded_repository_check(&self, source_id: i64, checked_at: i64) -> CachedUpdateCheck {
        let source = self
            .sources
            .iter()
            .find(|source| source.id() == source_id)
            .expect("an update plan retains its registered source");
        CachedUpdateCheck {
            source_id,
            checked_at,
            local_revision: source.head().to_owned(),
            // Nothing was written, so the only reference available is the one
            // the source records, which Git printed in its shortest unambiguous
            // spelling. `CachedUpdateCheck::superseded_by` compares the two over
            // every spelling rather than by rebuilding one from the other.
            local_reference: source
                .branch()
                .map(|branch| format!("refs/heads/{branch}")),
            upstream_ref: None,
            upstream_revision: None,
            merge_base: None,
            ahead: 0,
            behind: 0,
            dirty: source.dirty().unwrap_or(false),
            dirty_known: source.dirty().is_some(),
            verdict: RepositoryUpdateVerdict::Blocked,
            detail: "source.changed_after_preview|repository state changed after the preview; check updates again"
                .into(),
        }
    }

    /// Only a preview of executable work that has been read to its end accepts
    /// a confirmation.
    ///
    /// A blocked plan and a plan with nothing left to do both stay on screen
    /// rather than turning into a report of an install that never ran.
    fn confirm_operation(&mut self) -> Vec<Effect> {
        if self.metadata_failure().is_some() {
            return Vec::new();
        }
        match &self.pending_operation {
            Some(OperationPrompt::Install(InstallPrompt::Preview(plan)))
                if plan.is_executable() && self.operation_preview_fully_seen() =>
            {
                vec![Effect::ApplyInstall]
            }
            Some(OperationPrompt::Uninstall(UninstallPrompt::Preview(plan)))
                if plan.is_executable() && self.operation_preview_fully_seen() =>
            {
                vec![Effect::ApplyUninstall]
            }
            Some(OperationPrompt::Forget(ForgetPrompt::Preview(plan)))
                if plan.is_executable() && self.operation_preview_fully_seen() =>
            {
                vec![Effect::ApplyForgetSource]
            }
            _ => Vec::new(),
        }
    }

    /// Whether every row of the open preview has been on screen.
    ///
    /// Nothing is written until a plan the user has *seen* in full is
    /// confirmed, and a dialog taller than the terminal is not seen in full by
    /// being opened. The extent is the last frame's own measurement, so this is
    /// a fact about the terminal the reader is looking at rather than about the
    /// plan; a preview that always fitted is fully seen at rest, which is why
    /// the ordinary case costs no keystrokes at all.
    pub fn operation_preview_fully_seen(&self) -> bool {
        self.detail_measured && self.detail_scroll >= self.detail_max_scroll
    }

    /// Compatibility name retained for the repair callers and tests.
    pub fn preview_fully_seen(&self) -> bool {
        self.operation_preview_fully_seen()
    }

    fn confirm_repair(&mut self) -> Vec<Effect> {
        match &self.pending_repair {
            Some(RepairPrompt::Preview(plan))
                if plan.is_executable() && self.preview_fully_seen() =>
            {
                vec![Effect::ApplyRepair]
            }
            _ => Vec::new(),
        }
    }

    fn build_repair_preview(&mut self) -> RepairPrompt {
        // Capture the requested installation before refreshing: the source
        // refresh also restates and reorders Doctor, but the key must keep
        // answering for the row on which the user pressed it.
        let Some(entry) = self.selected_finding() else {
            return RepairPrompt::Failed("no Doctor finding is selected".to_owned());
        };
        let Some(observation) = entry.observation() else {
            return RepairPrompt::Failed(
                "the selected finding concerns the registry rather than an installed link"
                    .to_owned(),
            );
        };
        let skill_name = observation.name().to_owned();
        let agent = observation.agent();

        // RegisteredSource owns the candidates discovered by its last scan.
        // Repair re-resolves a receipt against what the registry offers now,
        // so a long-running TUI must refresh those candidates at the planning
        // boundary. The inventory and receipt overlay are rebuilt from the
        // same refreshed vector so the preview and its later verification do
        // not disagree about registry state.
        let sources = match self.store().and_then(Store::registered_sources) {
            Ok(sources) => sources,
            Err(error) => {
                return RepairPrompt::Failed(format!(
                    "registered sources could not be refreshed before repair: {error}"
                ));
            }
        };
        self.sources = sources;
        self.rescan_installations();

        match self.plan_repair_for(&skill_name, agent) {
            Ok(plan) => RepairPrompt::Preview(plan),
            Err(failure) => RepairPrompt::Failed(failure.message().to_owned()),
        }
    }

    /// Read the machine and decide what installing the focused variant would
    /// do.
    ///
    /// A failure here becomes something the dialog states rather than an error
    /// out of `perform_effects`, which would end the process: the user asked a
    /// question about their machine and is owed the answer, not an exit.
    fn build_install_preview(&self) -> std::result::Result<InstallPrompt, MetadataFailure> {
        let Some(SourceRow::Variant { catalog, candidate }) = self.selected_variant_row() else {
            return Ok(InstallPrompt::Failed(
                "the focused row is not a skill variant, so there is nothing to install".to_owned(),
            ));
        };
        let Some(source) = self.selected_source() else {
            return Ok(InstallPrompt::Failed("no source is selected".to_owned()));
        };
        let variant = VariantRef::of(source, catalog, candidate);
        match self.plan_install_for(&variant, [true; 3]) {
            Ok(plan) => Ok(InstallPrompt::Preview(plan)),
            // The dialog states either, because either is the answer to the
            // question the user asked. Only a caller that has to choose an exit
            // status needs them apart.
            Err(PlanRequestFailure::Unplannable(message)) => Ok(InstallPrompt::Failed(message)),
            Err(PlanRequestFailure::Metadata(failure)) => Err(failure),
        }
    }

    fn build_uninstall_preview(&self) -> UninstallPrompt {
        let Some(row) = self.selected_installation() else {
            return UninstallPrompt::Failed("no installed skill is selected".to_owned());
        };
        match self.plan_uninstall_for(row.name(), [true; 3]) {
            Ok(plan) => UninstallPrompt::Preview(plan),
            Err(failure) => UninstallPrompt::Failed(failure.message().to_owned()),
        }
    }

    fn build_forget_preview(&self) -> ForgetPrompt {
        let Some(source) = self.selected_source() else {
            return ForgetPrompt::Failed("no source is selected".to_owned());
        };
        let receipts = match self.store().and_then(Store::receipts) {
            Ok(receipts) => receipts,
            Err(error) => {
                return ForgetPrompt::Preview(plan_forget_unreadable_receipts(
                    source,
                    error.to_string(),
                ));
            }
        };
        let probe = probe_forget(source, &receipts);
        ForgetPrompt::Preview(plan_forget(source, &receipts, &probe))
    }

    /// The one planning path shared by the screen and `skilled uninstall`.
    pub(crate) fn plan_uninstall_for(
        &self,
        skill_name: &str,
        requested: [bool; 3],
    ) -> std::result::Result<UninstallPlan, PlanRequestFailure> {
        if !valid_skill_name(skill_name) {
            return Err(PlanRequestFailure::Unplannable(
                "the uninstall skill name must be 1-64 lowercase ASCII letters or digits with single hyphen separators"
                    .to_owned(),
            ));
        }
        let receipts = self.store().and_then(Store::receipts).map_err(|error| {
            PlanRequestFailure::Metadata(MetadataFailure::new(
                self.metadata_database_path(),
                format!(
                    "the ownership receipts could not be read, so Skilled cannot tell its own \
                     links from anyone else's: {error}"
                ),
            ))
        })?;
        let probe = probe_uninstall(&self.agents, skill_name, self.home());
        Ok(plan_uninstall(
            &self.agents,
            &receipts,
            skill_name,
            requested,
            &probe,
        ))
    }

    /// Decide what installing one variant would do, for one set of agents.
    ///
    /// The one place planning happens: the Sources flow and `skilled install`
    /// go through it together, so the command line cannot end up applying a
    /// different set of checks from the screen.
    pub(crate) fn plan_install_for(
        &self,
        variant: &VariantRef,
        requested: [bool; 3],
    ) -> std::result::Result<InstallPlan, PlanRequestFailure> {
        let store = match &self.metadata {
            Metadata::Ready(store) => store,
            Metadata::Unavailable(failure) => {
                return Err(PlanRequestFailure::Metadata(failure.clone()));
            }
        };
        let receipts = store.receipts().map_err(|error| {
            PlanRequestFailure::Metadata(MetadataFailure::new(
                store.database_path().to_path_buf(),
                format!(
                    "the ownership receipts could not be read, so Skilled cannot tell its own \
                     links from anyone else\'s: {error}"
                ),
            ))
        })?;
        let probe = probe_install(&self.agents, &self.sources, variant, self.home());
        plan_install(
            &self.agents,
            &self.sources,
            variant,
            requested,
            &probe,
            &receipts,
        )
        .map_err(|failure| PlanRequestFailure::Unplannable(failure.to_string()))
    }

    /// Build the same single-target repair plan used by Doctor and the CLI.
    pub(crate) fn plan_repair_for(
        &self,
        skill_name: &str,
        agent: AgentKind,
    ) -> std::result::Result<RepairPlan, PlanRequestFailure> {
        let receipts = self.store().and_then(Store::receipts).map_err(|error| {
            PlanRequestFailure::Metadata(MetadataFailure::new(
                self.metadata_database_path(),
                format!(
                    "the ownership receipts could not be read, so Skilled cannot prove this link \
                     is its own: {error}"
                ),
            ))
        })?;
        let probe = probe_repair(&self.agents, &self.sources, skill_name, agent, self.home());
        Ok(plan_repair(
            &self.agents,
            &self.sources,
            skill_name,
            agent,
            &probe,
            &receipts,
        ))
    }

    /// Apply a plan, restate the inventory, and check the plan against it.
    ///
    /// The same three steps [`Effect::ApplyInstall`] performs, in the same
    /// order and for the same reason.
    pub(crate) fn apply_plan(&mut self, plan: &InstallPlan) -> Result<InstallOutcome> {
        let home = self.environment.home_dir.clone();
        let applied = match &mut self.metadata {
            Metadata::Ready(store) => apply_install(plan, store, &home),
            Metadata::Unavailable(failure) => {
                return Err(Error::MetadataUnavailable(failure.clone()));
            }
        };
        if let Some(failure) = applied.metadata_failure().cloned() {
            self.set_degraded(failure);
        }
        self.rescan_installations();
        let verification = verify_install(plan, &applied, &self.inventory);
        Ok(InstallOutcome::new(plan.clone(), applied, verification))
    }

    pub(crate) fn apply_uninstall_plan(
        &mut self,
        plan: &UninstallPlan,
    ) -> Result<UninstallOutcome> {
        if let Some(failure) = self.metadata_failure() {
            return Err(Error::MetadataUnavailable(failure.clone()));
        }
        let home = self.environment.home_dir.clone();
        let applied = match &self.metadata {
            Metadata::Ready(store) => apply_uninstall(plan, store, &home),
            Metadata::Unavailable(_) => unreachable!("metadata readiness was settled above"),
        };
        let content = probe_uninstall_content(plan);
        self.rescan_installations();
        let verification = verify_uninstall(plan, &applied, &self.inventory, &content);
        let finalized = match &mut self.metadata {
            Metadata::Ready(store) => finalize_uninstall(plan, &applied, &verification, store),
            Metadata::Unavailable(_) => unreachable!("nothing between here degrades the session"),
        };
        self.refresh_receipts();
        Ok(UninstallOutcome::new(
            plan.clone(),
            applied,
            verification,
            finalized,
        ))
    }

    pub(crate) fn apply_forget_plan(&mut self, plan: &ForgetPlan) -> Result<ForgetOutcome> {
        if let Some(failure) = self.metadata_failure() {
            return Err(Error::MetadataUnavailable(failure.clone()));
        }
        let applied = match &mut self.metadata {
            Metadata::Ready(store) => apply_forget(plan, store),
            Metadata::Unavailable(_) => unreachable!("metadata readiness was settled above"),
        };
        let verification = match &applied {
            crate::operations::ForgetApply::Forgotten
            | crate::operations::ForgetApply::NothingToDo => {
                let Metadata::Ready(store) = &self.metadata else {
                    unreachable!("nothing between here degrades the session")
                };
                verify_forget(plan, store)
            }
            crate::operations::ForgetApply::Failed(_) => {
                crate::operations::ForgetVerification::Withheld(
                    "metadata postconditions were not checked because the forget operation did not run"
                        .to_owned(),
                )
            }
        };
        if matches!(
            applied,
            crate::operations::ForgetApply::Forgotten | crate::operations::ForgetApply::NothingToDo
        ) {
            self.sources = match self.store().and_then(Store::registered_sources) {
                Ok(sources) => sources,
                Err(_) => self
                    .sources
                    .iter()
                    .filter(|source| source.id() != plan.source().id())
                    .cloned()
                    .collect(),
            };
            self.focused_source = self
                .focused_source
                .min(self.sources.len().saturating_sub(1));
            self.focused_variant = 0;
            self.rescan_installations();
        }
        self.refresh_receipts();
        Ok(ForgetOutcome::new(plan.clone(), applied, verification))
    }

    pub(crate) fn apply_repair_plan(&mut self, plan: &RepairPlan) -> Result<RepairOutcome> {
        let home = self.environment.home_dir.clone();
        let applied = match &mut self.metadata {
            Metadata::Ready(store) => apply_repair(plan, store, &home),
            Metadata::Unavailable(failure) => {
                return Err(Error::MetadataUnavailable(failure.clone()));
            }
        };
        self.rescan_installations();
        let verification = verify_repair(plan, &applied, &self.inventory);
        Ok(RepairOutcome::new(plan.clone(), applied, verification))
    }

    /// Apply the shown preview, then restate the inventory and check the plan
    /// against it.
    ///
    /// The rescan happens before verification and not after, because the scan
    /// is the evidence verification rests on; and it happens whatever the apply
    /// did, so the inventory left behind describes the machine as it now is.
    fn apply_pending_install(&mut self) {
        let Some(OperationPrompt::Install(InstallPrompt::Preview(plan))) =
            self.pending_operation.take()
        else {
            return;
        };
        match self.apply_plan(&plan) {
            Ok(outcome) => {
                self.pending_operation =
                    Some(OperationPrompt::Install(InstallPrompt::Report(outcome)));
            }
            Err(Error::MetadataUnavailable(failure)) => self.degrade(failure),
            Err(_) => unreachable!("apply_plan only refuses unavailable metadata"),
        }
        // The report is different content from the preview it replaced.
        self.reset_detail_scroll();
    }

    fn apply_pending_uninstall(&mut self) {
        let Some(OperationPrompt::Uninstall(UninstallPrompt::Preview(plan))) =
            self.pending_operation.take()
        else {
            return;
        };
        match self.apply_uninstall_plan(&plan) {
            Ok(outcome) => {
                self.pending_operation =
                    Some(OperationPrompt::Uninstall(UninstallPrompt::Report(outcome)));
            }
            Err(Error::MetadataUnavailable(failure)) => self.degrade(failure),
            Err(_) => unreachable!("apply_uninstall_plan only refuses unavailable metadata"),
        }
        self.reset_detail_scroll();
    }

    fn apply_pending_forget(&mut self) {
        let Some(OperationPrompt::Forget(ForgetPrompt::Preview(plan))) =
            self.pending_operation.take()
        else {
            return;
        };
        match self.apply_forget_plan(&plan) {
            Ok(outcome) => {
                self.pending_operation =
                    Some(OperationPrompt::Forget(ForgetPrompt::Report(outcome)));
            }
            Err(Error::MetadataUnavailable(failure)) => self.degrade(failure),
            Err(_) => unreachable!("apply_forget_plan only refuses unavailable metadata"),
        }
        self.reset_detail_scroll();
    }

    fn apply_pending_repair(&mut self) {
        let Some(RepairPrompt::Preview(plan)) = self.pending_repair.take() else {
            return;
        };
        match self.apply_repair_plan(&plan) {
            Ok(outcome) => self.pending_repair = Some(RepairPrompt::Report(outcome)),
            Err(Error::MetadataUnavailable(failure)) => self.degrade(failure),
            Err(_) => unreachable!("apply_repair_plan only refuses unavailable metadata"),
        }
        self.reset_detail_scroll();
    }

    /// Replace the inventory with a fresh read-only pass over the native roots.
    ///
    /// This is the only place installation scanning happens; the reducer stays
    /// free of filesystem work.
    fn rescan_installations(&mut self) {
        self.inventory =
            scan_installations(&self.agents, &self.sources, self.registry_availability);
        // One read serves both readers of the receipt table: the overlay needs
        // to say that it could not be read, and the cache keeps the last
        // readable answer rather than flattening a failure to empty. A
        // degraded session has no store to ask, which is a reason the overlay
        // cannot tell rather than a reason to claim there are no receipts.
        self.repair_overlay = match self.receipts() {
            Ok(receipts) => {
                let overlay =
                    RepairOverlay::build(&self.inventory, &receipts, &self.sources, &self.agents);
                self.cached_receipts = Some(receipts);
                overlay
            }
            // A degraded session names its database in the banner above every
            // screen. Repeating the path inside a detail field would say the
            // same thing twice, in the narrowest column on the screen.
            Err(Error::MetadataUnavailable(_)) => RepairOverlay::receipts_unread(
                "the application metadata is unavailable this session".to_owned(),
            ),
            Err(error) => RepairOverlay::receipts_unread(error.to_string()),
        };
        self.refilter_installations();
        // The findings behind the selection are gone with the snapshot, so the
        // selection is pulled back onto the list that replaced them.
        let last = self.finding_count().saturating_sub(1);
        self.focused_finding = self.focused_finding.min(last);
    }

    /// Refresh ownership evidence without flattening a failed read to empty.
    fn refresh_receipts(&mut self) {
        if let Ok(receipts) = self.store().and_then(Store::receipts) {
            self.cached_receipts = Some(receipts);
        }
    }

    /// Recompute which rows the query admits, and keep the selection on one.
    ///
    /// A row matches when the query appears in its name, the label of its
    /// provenance or of any installation's own provenance, or the word the
    /// Health column states for it, so the same box narrows by identity,
    /// provenance, or state — including the states that come from OpenCode's
    /// effective resolution rather than from any one installation.
    /// Matching each installation keeps a mixed row findable by the source it
    /// partly came from and by "not registered" alike; stray content is not an
    /// installation and answers for no provenance.
    fn refilter_installations(&mut self) {
        let needle = self.inventory_filter.trim().to_lowercase();
        self.filtered_installations = self
            .inventory
            .rows()
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                needle.is_empty()
                    || row.name().to_lowercase().contains(&needle)
                    || row.provenance().label().to_lowercase().contains(&needle)
                    || row.observations().any(|observation| {
                        observation.object().is_installation()
                            && observation
                                .provenance()
                                .label()
                                .to_lowercase()
                                .contains(&needle)
                    })
                    || row.verdict().label().contains(needle.as_str())
            })
            .map(|(index, _)| index)
            .collect();
        let last = self.filtered_installations.len().saturating_sub(1);
        self.focused_installation = self.focused_installation.min(last);
        self.reset_detail_scroll();
    }

    /// Return the detail region's window to the top, and forget the extent
    /// measured for content that is no longer there.
    ///
    /// Both halves matter: an offset kept across a change would point into a
    /// skill the user never scrolled through, and an extent kept across one
    /// would let the next keystroke scroll past the end of shorter content
    /// before a frame has had the chance to measure it.
    fn reset_detail_scroll(&mut self) {
        self.detail_scroll = 0;
        self.detail_max_scroll = 0;
        self.detail_measured = false;
        self.update_preview_fully_seen = false;
    }

    /// Open Doctor on a scan taken for Doctor.
    ///
    /// The same contract [`Self::enter_inventory`] keeps, for the same reason:
    /// the findings this view lists must have been observed for this view, not
    /// inherited from whichever one the user was standing in.
    fn enter_doctor(&mut self) -> Vec<Effect> {
        self.view = View::Doctor;
        self.doctor_pane = DoctorPane::Findings;
        self.inventory = InventorySnapshot::not_scanned(&self.agents, self.registry_availability);
        self.repair_overlay = RepairOverlay::default();
        self.filtered_installations.clear();
        self.reset_detail_scroll();
        vec![Effect::ScanInstallations]
    }

    fn enter_inventory(&mut self) -> Vec<Effect> {
        self.view = View::Inventory;
        self.inventory_pane = InventoryPane::Skills;
        // The scan is the effect that follows, so between the transition and
        // the effect there is no scan for this view. Say so rather than rest
        // on one taken for the view just left: whatever is rendered beside
        // the Inventory was observed for the Inventory. Deselected roots are
        // NotSelected in that gap too — the scan will never read them. The
        // runner performs effects before drawing, so no frame of this state
        // reaches a user today; the reset is what keeps the reducer honest at
        // every instant should that ever change.
        self.inventory = InventorySnapshot::not_scanned(&self.agents, self.registry_availability);
        self.repair_overlay = RepairOverlay::default();
        // The gap snapshot holds no rows, so the only consistent filtered
        // list is empty whatever the query. Clearing it directly — rather
        // than refiltering — leaves the focused row alone: the scan that
        // lands refilters and re-clamps it against the fresh rows, so a row
        // that is still there keeps its selection.
        self.filtered_installations.clear();
        // The rows this window was scrolled through are gone with the
        // snapshot, and the scan that lands refilters, which resets it again.
        self.reset_detail_scroll();
        vec![Effect::ScanInstallations]
    }

    /// Apply one keystroke to the open filter bar.
    ///
    /// The query narrows the list as it is typed, so `Enter` only hands the
    /// keyboard back and `Esc` clears the query as it closes.
    fn filter_input(&mut self, action: Action) {
        match action {
            Action::AppendInventoryFilter(character)
                if !character.is_control()
                    && self.inventory_filter.chars().count() < MAX_INVENTORY_FILTER =>
            {
                self.inventory_filter.push(character);
            }
            Action::DeleteInventoryFilterCharacter => {
                self.inventory_filter.pop();
            }
            Action::SubmitInventoryFilter => self.inventory_filter_active = false,
            Action::Back | Action::CancelSourceFlow => {
                self.inventory_filter.clear();
                self.inventory_filter_active = false;
            }
            _ => return,
        }
        self.refilter_installations();
    }

    fn move_installation_selection(&mut self, delta: i8) {
        if self.view != View::Inventory || self.inventory_pane != InventoryPane::Skills {
            return;
        }
        self.focused_installation = wrapped_index(
            self.focused_installation,
            delta,
            self.filtered_installations.len(),
        );
        self.reset_detail_scroll();
    }

    /// Move the detail region's window, clamped rather than wrapped.
    ///
    /// A list wraps because every row is a place to stand; a window does not,
    /// because the top and the bottom of a document are ends rather than
    /// neighbours.
    fn scroll_detail(&mut self, delta: i8) {
        if !self.detail_region_has_the_keyboard() {
            return;
        }
        let offset = self.detail_scroll.saturating_add_signed(isize::from(delta));
        self.detail_scroll = offset.min(self.detail_max_scroll);
    }

    /// Whether the focused region is a detail region, whichever screen it is on.
    ///
    /// The offset is one piece of state because only one detail region is ever
    /// drawn: a window belongs to the content under it, and moving between
    /// screens replaces that content.
    fn detail_region_has_the_keyboard(&self) -> bool {
        // A modal dialog is drawn over whatever screen is behind it and takes
        // the keyboard with it, so while one is open it is the window the
        // movement keys move.
        if self.pending_operation.is_some()
            || self.pending_repair.is_some()
            || self.pending_update.is_some()
        {
            return true;
        }
        match self.view {
            View::Inventory => self.inventory_pane == InventoryPane::Details,
            View::Updates => self.updates_pane == UpdatesPane::Details,
            View::Doctor => self.doctor_pane == DoctorPane::Details,
            _ => false,
        }
    }

    pub fn open_settings(&mut self) {
        if self.view == View::Inventory {
            self.view = View::Settings;
        }
    }

    fn rerun_setup(&mut self) -> Vec<Effect> {
        if self.view == View::Settings {
            self.view = View::Setup(SetupStep::Welcome);
            return vec![
                Effect::ResetSetup,
                Effect::RedetectAgents {
                    agent_selections: self.agents.each_ref().map(|agent| agent.selected()),
                },
            ];
        }
        Vec::new()
    }

    fn back(&mut self) -> UpdateResult {
        if self.source_path_input_active || self.pending_source.is_some() {
            if self.view == View::Setup(SetupStep::ConfirmCatalogs) {
                self.view = View::Setup(SetupStep::DiscoverSources);
            }
            self.clear_pending_source_state();
            return UpdateResult::continuing(Vec::new());
        }
        match self.view {
            View::Setup(step) => {
                if let Some(previous) = step.previous() {
                    self.view = View::Setup(previous);
                }
            }
            View::Settings => return UpdateResult::continuing(self.enter_inventory()),
            // Back unwinds the drilled-in region first, then the view.
            View::Doctor => match self.doctor_pane {
                DoctorPane::Details => self.doctor_pane = DoctorPane::Findings,
                DoctorPane::Findings => {
                    return UpdateResult::continuing(self.enter_inventory());
                }
            },
            View::Sources => match self.sources_pane {
                SourcesPane::Details => self.sources_pane = SourcesPane::Variants,
                SourcesPane::Variants => self.sources_pane = SourcesPane::Repositories,
                SourcesPane::Repositories => {
                    return UpdateResult::continuing(self.enter_inventory());
                }
            },
            View::Updates => match self.updates_pane {
                UpdatesPane::Details => self.updates_pane = UpdatesPane::Candidates,
                UpdatesPane::Candidates => return UpdateResult::continuing(self.enter_inventory()),
            },
            // Back unwinds the narrowest thing first: an applied filter, then
            // a drilled-in detail region.
            View::Inventory => {
                if !self.inventory_filter.is_empty() {
                    self.inventory_filter.clear();
                    self.refilter_installations();
                } else if self.inventory_pane == InventoryPane::Details {
                    self.inventory_pane = InventoryPane::Skills;
                }
            }
        }
        UpdateResult::continuing(Vec::new())
    }

    fn clear_pending_source_state(&mut self) {
        self.source_path_input_active = false;
        self.source_path.clear();
        self.source_error = None;
        self.pending_source = None;
        self.focused_catalog = 0;
    }

    fn move_selection(&mut self, delta: i8) {
        if self.view != View::Setup(SetupStep::DetectAgents) {
            return;
        }
        self.focused_agent = wrapped_index(self.focused_agent, delta, self.agents.len());
    }

    fn toggle_selection(&mut self) {
        if self.view == View::Setup(SetupStep::DetectAgents) {
            self.agents[self.focused_agent].toggle_selected();
        }
    }

    fn advance_setup(&mut self) -> Vec<Effect> {
        if self.metadata_failure().is_some() {
            return Vec::new();
        }
        let View::Setup(step) = self.view else {
            return Vec::new();
        };

        if step == SetupStep::ConfirmCatalogs && self.pending_source.is_some() {
            return self.register_pending_source();
        }

        if step == SetupStep::DetectAgents {
            // The user has now chosen the scope this session will scan. It is
            // truthful even before persistence and survives a later metadata
            // failure during the remaining setup steps.
            self.scan_scope_known = true;
        }

        match step.next() {
            Some(next) => {
                self.view = View::Setup(next);
                // Step six reports what is installed, and the summary counts
                // it, so both are backed by a scan taken on arrival.
                if next == SetupStep::ScanInstallations {
                    return vec![Effect::ScanInstallations];
                }
                Vec::new()
            }
            None => {
                let mut effects = self.enter_inventory();
                effects.push(Effect::PersistSetup {
                    agent_selections: self.agents.each_ref().map(|agent| agent.selected()),
                });
                effects
            }
        }
    }

    fn submit_source_path(&self) -> Vec<Effect> {
        if !self.source_path_input_active || self.source_path.trim().is_empty() {
            return Vec::new();
        }
        vec![Effect::InspectSource {
            path: PathBuf::from(&self.source_path),
        }]
    }

    fn register_pending_source(&mut self) -> Vec<Effect> {
        let Some(preview) = self.pending_source.clone() else {
            return Vec::new();
        };
        if !preview.has_included_catalog() {
            self.source_error = Some("Select at least one catalog root to register.".to_owned());
            return Vec::new();
        }
        vec![Effect::RegisterSource { preview }]
    }

    fn move_sources_selection(&mut self, delta: i8) {
        if self.view != View::Sources {
            return;
        }
        match self.sources_pane {
            SourcesPane::Repositories if !self.sources.is_empty() => {
                let next = wrapped_index(self.focused_source, delta, self.sources.len());
                if next != self.focused_source {
                    self.focused_source = next;
                    self.focused_variant = 0;
                }
            }
            SourcesPane::Variants => {
                let count = self.variants_row_count();
                self.focused_variant = wrapped_index(self.focused_variant, delta, count);
            }
            SourcesPane::Repositories | SourcesPane::Details => {}
        }
    }

    fn focus_registered_source(&mut self, path: &Path) {
        self.focused_source = self
            .sources
            .iter()
            .position(|source| source.git_top_level() == path)
            .unwrap_or(0);
        self.focused_variant = 0;
    }
}

fn publish_update_worker_failure(
    terminal_state: &AtomicU8,
    sender: &mpsc::Sender<UpdateCheckMessage>,
) {
    if terminal_state
        .compare_exchange(
            UPDATE_CHECK_RUNNING,
            UPDATE_CHECK_FINISHED,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
    {
        let _ = sender.send(UpdateCheckMessage::Failed(
            "update check worker panicked before completing".to_owned(),
        ));
    } else if terminal_state.load(Ordering::Acquire) == UPDATE_CHECK_CANCELLED {
        let _ = sender.send(UpdateCheckMessage::Cancelled);
    }
}

/// Letting go of the application ends the check it started.
///
/// The worker owns nothing that would stop it: it ignores failed sends, so a
/// dropped receiver is not a signal, and a run reached through an error path
/// rather than through Quit would otherwise go on fetching every registered
/// source after the terminal had been handed back. Cancelling and joining here
/// is what keeps network work inside the lifetime of the application that
/// asked for it.
impl Drop for SkilledApp {
    fn drop(&mut self) {
        let Some(run) = self.update_check_run.take() else {
            return;
        };
        if run
            .terminal_state
            .compare_exchange(
                UPDATE_CHECK_RUNNING,
                UPDATE_CHECK_CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            run.cancelled.store(true, Ordering::Release);
        }
        if let Some(mut child) = run
            .child
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
        {
            crate::git::terminate_child(&mut child);
        }
        let _ = run.handle.join();
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use super::*;

    fn git(repository: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .output()
            .expect("run git fixture command");
        assert!(output.status.success(), "git {arguments:?} failed");
    }

    fn test_app() -> (tempfile::TempDir, SkilledApp) {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let app = SkilledApp::open(AppEnvironment::new(
            temporary.path().join("home"),
            temporary.path().join("data"),
            "",
        ))
        .expect("open app");
        (temporary, app)
    }

    #[test]
    fn cancelling_honours_a_finished_message_already_in_the_queue() {
        let (_temporary, mut app) = test_app();
        let (sender, receiver) = mpsc::channel();
        sender
            .send(UpdateCheckMessage::Finished(Vec::new()))
            .expect("queue finished result");
        let cancelled = Arc::new(AtomicBool::new(false));
        let observed_cancelled = Arc::clone(&cancelled);
        app.update_check_run = Some(UpdateCheckRun {
            receiver,
            handle: std::thread::spawn(|| {}),
            cancelled,
            terminal_state: Arc::new(AtomicU8::new(UPDATE_CHECK_FINISHED)),
            child: Arc::new(Mutex::new(None)),
        });

        app.cancel_update_check();

        assert!(!observed_cancelled.load(Ordering::Acquire));
        assert!(!app.update_check_in_flight());
        assert!(app.update_check_error().is_none());
    }

    #[test]
    fn draining_honours_a_finished_message_that_won_the_cancellation_race() {
        let (_temporary, mut app) = test_app();
        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let terminal_state = Arc::new(AtomicU8::new(UPDATE_CHECK_RUNNING));
        let worker_state = Arc::clone(&terminal_state);
        let (claimed_sender, claimed_receiver) = mpsc::sync_channel(0);
        let (release_sender, release_receiver) = mpsc::sync_channel(0);
        let handle = std::thread::spawn(move || {
            worker_state
                .compare_exchange(
                    UPDATE_CHECK_RUNNING,
                    UPDATE_CHECK_FINISHED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .expect("claim finished result");
            claimed_sender.send(()).expect("publish completion claim");
            release_receiver.recv().expect("release finished message");
            sender
                .send(UpdateCheckMessage::Finished(Vec::new()))
                .expect("queue finished result");
        });
        app.update_check_run = Some(UpdateCheckRun {
            receiver,
            handle,
            cancelled: Arc::clone(&cancelled),
            terminal_state,
            child: Arc::new(Mutex::new(None)),
        });

        claimed_receiver.recv().expect("worker claimed completion");
        app.cancel_update_check();
        assert!(!cancelled.load(Ordering::Acquire));
        release_sender.send(()).expect("publish finished message");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let effects = loop {
            let effects = app.drain_update_check();
            if !effects.is_empty() || std::time::Instant::now() >= deadline {
                break effects;
            }
            std::thread::yield_now();
        };

        assert_eq!(
            effects,
            [
                Effect::RecordUpdateChecks(Vec::new()),
                Effect::FinishUpdateCheck
            ]
        );
    }

    #[test]
    fn cancellation_suppresses_a_later_worker_panic_publication() {
        let terminal_state = AtomicU8::new(UPDATE_CHECK_CANCELLED);
        let (sender, receiver) = mpsc::channel();

        publish_update_worker_failure(&terminal_state, &sender);

        assert!(matches!(
            receiver.try_recv(),
            Ok(UpdateCheckMessage::Cancelled)
        ));
    }

    #[test]
    fn cancelling_retains_a_running_worker_until_it_has_stopped() {
        let (_temporary, mut app) = test_app();
        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let handle = std::thread::spawn(move || {
            while !worker_cancelled.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            drop(sender);
        });
        app.update_check_run = Some(UpdateCheckRun {
            receiver,
            handle,
            cancelled,
            terminal_state: Arc::new(AtomicU8::new(UPDATE_CHECK_RUNNING)),
            child: Arc::new(Mutex::new(None)),
        });

        app.cancel_update_check();

        assert!(app.update_check_in_flight());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while app.update_check_in_flight() && std::time::Instant::now() < deadline {
            let effects = app.drain_update_check();
            app.perform_effects(&effects)
                .expect("retire cancelled worker");
            std::thread::yield_now();
        }
        assert!(!app.update_check_in_flight());
    }

    /// A recorded check time also orders writes, so a clock that moves
    /// backwards must not be able to hand out one the store already holds: the
    /// conditional upsert would reject the newer check, report success, and go
    /// on serving the stale one.
    #[test]
    fn a_recorded_check_time_never_repeats_or_falls_back() {
        let first = now();
        let second = now();
        assert!(second > first, "{first} then {second}");

        note_generation(second + 5);

        assert!(now() > second + 5);
    }

    #[test]
    fn repository_update_windows_own_detail_scrolling() {
        let (_temporary, mut app) = test_app();
        app.detail_max_scroll = 4;
        app.pending_update = Some(RepositoryUpdatePrompt::Failed("fixture".into()));

        app.scroll_detail(1);

        assert_eq!(app.detail_scroll(), 1);
        app.pending_update = None;
        app.view = View::Updates;
        app.updates_pane = UpdatesPane::Details;
        app.scroll_detail(1);
        assert_eq!(app.detail_scroll(), 2);
    }

    #[test]
    fn a_disconnected_update_worker_surfaces_a_terminal_error() {
        let (_temporary, mut app) = test_app();
        let (sender, receiver) = mpsc::channel();
        drop(sender);
        app.update_check_run = Some(UpdateCheckRun {
            receiver,
            handle: std::thread::spawn(|| {}),
            cancelled: Arc::new(AtomicBool::new(false)),
            terminal_state: Arc::new(AtomicU8::new(UPDATE_CHECK_RUNNING)),
            child: Arc::new(Mutex::new(None)),
        });

        let effects = app.drain_update_check();

        assert_eq!(effects, [Effect::FinishUpdateCheck]);
        assert!(
            app.update_check_error()
                .is_some_and(|error| error.contains("ended before completing"))
        );
    }

    /// A worker ignores failed sends, so a dropped receiver tells it nothing.
    /// Nothing but the cancellation flag can end a check the application is no
    /// longer there to receive.
    #[test]
    fn dropping_the_application_cancels_an_update_check() {
        let (_temporary, mut app) = test_app();
        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let observed = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let worker_observed = Arc::clone(&observed);
        let handle = std::thread::spawn(move || {
            // Bounded so a regression fails the assertion below rather than
            // hanging the suite on a join that never returns.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                if worker_cancelled.load(Ordering::Acquire) {
                    worker_observed.store(true, Ordering::Release);
                    break;
                }
                std::thread::yield_now();
            }
            let _ = sender.send(UpdateCheckMessage::Cancelled);
        });
        app.update_check_run = Some(UpdateCheckRun {
            receiver,
            handle,
            cancelled: Arc::clone(&cancelled),
            terminal_state: Arc::new(AtomicU8::new(UPDATE_CHECK_RUNNING)),
            child: Arc::new(Mutex::new(None)),
        });

        drop(app);

        assert!(cancelled.load(Ordering::Acquire));
        assert!(observed.load(Ordering::Acquire));
    }

    #[test]
    fn update_check_persistence_failures_have_a_dedicated_terminal_error() {
        let (_temporary, mut app) = test_app();
        let check = CachedUpdateCheck {
            source_id: 99,
            checked_at: 0,
            local_revision: "abc".into(),
            local_reference: None,
            upstream_ref: None,
            upstream_revision: None,
            merge_base: None,
            ahead: 0,
            behind: 0,
            dirty: false,
            dirty_known: true,
            verdict: RepositoryUpdateVerdict::UpToDate,
            detail: String::new(),
        };

        app.persist_update_checks(&[check]);

        assert!(
            app.update_check_error()
                .is_some_and(|error| error.contains("could not be saved"))
        );
        assert!(app.update_checks().is_empty());
        assert!(app.source_error().is_none());
    }

    fn describe(row: &SourceRow<'_>) -> String {
        match row {
            SourceRow::CatalogError { error, .. } => format!("error: {error}"),
            SourceRow::Variant { candidate, .. } => candidate.directory_name().to_owned(),
            SourceRow::NoVariants(_) => "no variants".to_owned(),
        }
    }

    /// The scanner empties a catalog's candidates whenever it records an
    /// error, so the two never arrive together today. The order between them
    /// is stated anyway, and stated here, so that the day a scan reports what
    /// it managed to read alongside what defeated it, the pane does not
    /// silently start listing skills above the reason the list is short.
    #[test]
    fn a_catalog_holding_both_an_error_and_candidates_states_the_error_first() {
        let catalog = CatalogProposal::for_test(
            "skills",
            vec![
                SkillCandidate::for_test("first"),
                SkillCandidate::for_test("second"),
            ],
            Some("permission denied"),
        );

        let rows = catalog_rows(&catalog)
            .map(|row| describe(&row))
            .collect::<Vec<_>>();

        assert_eq!(rows, ["error: permission denied", "first", "second"]);
    }

    /// `no variants` belongs to a catalog that was read and holds nothing —
    /// never to one that could not be read, which says it is unreadable
    /// instead. Flattening the two would report an absence Skilled never
    /// observed.
    #[test]
    fn an_unreadable_catalog_never_also_claims_to_be_empty() {
        let unreadable = CatalogProposal::for_test("skills", Vec::new(), Some("permission denied"));
        let empty = CatalogProposal::for_test("skills", Vec::new(), None);

        assert_eq!(
            catalog_rows(&unreadable)
                .map(|row| describe(&row))
                .collect::<Vec<_>>(),
            ["error: permission denied"]
        );
        assert_eq!(
            catalog_rows(&empty)
                .map(|row| describe(&row))
                .collect::<Vec<_>>(),
            ["no variants"]
        );
    }

    #[test]
    fn a_mid_session_reset_failure_forces_read_only_inventory_without_propagating() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let mut app = SkilledApp::open(AppEnvironment::new(
            temporary.path().join("home"),
            temporary.path().join("data"),
            "",
        ))
        .expect("open application");
        for _ in 0..7 {
            let update = app.update(Action::Continue);
            app.perform_effects(update.effects())
                .expect("complete setup");
        }
        app.update(Action::OpenSettings);
        let update = app.update(Action::RerunSetup);
        app.fail_metadata_next(crate::store::MetadataOperation::ResetSetup);

        app.perform_effects(update.effects())
            .expect("metadata failure is recoverable");

        assert_eq!(app.view(), View::Inventory);
        assert!(app.metadata_failure().is_some());
        assert_eq!(app.registry_availability(), RegistryAvailability::Readable);
        assert!(app.scan_scope_known());
        assert!(!app.can_add_source());
        assert!(!app.can_rerun_setup());
        assert!(app.pending_source().is_none());
        assert!(!app.source_path_input_active());
    }

    #[test]
    fn a_setup_source_failure_retains_the_current_session_scan_scope() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let home = temporary.path().join("home");
        let repository = temporary.path().join("source");
        fs::create_dir_all(repository.join("skills/portable")).expect("create source fixture");
        fs::write(
            repository.join("skills/portable/SKILL.md"),
            "---\nname: portable\ndescription: portable fixture\n---\n",
        )
        .expect("write source fixture");
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "user.name", "Test Author"]);
        git(&repository, &["config", "user.email", "test@example.com"]);
        git(&repository, &["add", "."]);
        git(&repository, &["commit", "--quiet", "-m", "fixture"]);
        let hidden = home.join(".codex/skills/deselected");
        fs::create_dir_all(&hidden).expect("create deselected root fixture");
        fs::write(
            hidden.join("SKILL.md"),
            "---\nname: deselected\ndescription: must not be scanned\n---\n",
        )
        .expect("write deselected skill");

        let mut app = SkilledApp::open(AppEnvironment::new(
            &home,
            temporary.path().join("data"),
            "",
        ))
        .expect("open application");
        app.update(Action::Continue);
        app.update(Action::MoveSelection(1));
        app.update(Action::ToggleSelection);
        app.update(Action::MoveSelection(1));
        app.update(Action::ToggleSelection);
        app.update(Action::Continue);
        let preview = app.preview_source(&repository).expect("preview source");
        app.fail_metadata_next(crate::store::MetadataOperation::RegisterSource);

        let result = app.confirm_source(preview);

        assert!(matches!(result, Err(Error::MetadataUnavailable(_))));
        assert_eq!(app.view(), View::Inventory);
        assert!(app.scan_scope_known());
        assert!(app.agent(AgentKind::ClaudeCode).selected());
        assert!(!app.agent(AgentKind::Codex).selected());
        assert!(!app.agent(AgentKind::OpenCode).selected());
        assert!(app.inventory().row("deselected").is_none());
    }

    /// The store refusing one checkout path is not the store failing. Degrading
    /// is irreversible for the session, so a request it declines has to leave
    /// the next request — and every other write — still available.
    #[test]
    fn a_refused_checkout_path_does_not_degrade_the_session() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let home = temporary.path().join("home");
        let repository = temporary.path().join("source");
        fs::create_dir_all(repository.join("skills/portable")).expect("create source fixture");
        fs::write(
            repository.join("skills/portable/SKILL.md"),
            "---\nname: portable\ndescription: portable fixture\n---\n",
        )
        .expect("write source fixture");
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "user.name", "Test Author"]);
        git(&repository, &["config", "user.email", "test@example.com"]);
        git(&repository, &["add", "."]);
        git(&repository, &["commit", "--quiet", "-m", "fixture"]);

        let mut app = SkilledApp::open(AppEnvironment::new(
            &home,
            temporary.path().join("data"),
            "",
        ))
        .expect("open application");
        for _ in 0..7 {
            let update = app.update(Action::Continue);
            app.perform_effects(update.effects())
                .expect("complete setup");
        }
        let preview = app.preview_source(&repository).expect("preview source");
        app.fail_metadata_next(crate::store::MetadataOperation::RefuseSourceRequest);

        let refusal = app
            .confirm_source(preview)
            .expect_err("the checkout path is refused");

        assert!(matches!(refusal, Error::InvalidSourcePath(_)));
        assert!(app.metadata_failure().is_none(), "the session degraded");
        assert!(app.can_add_source());
        assert!(app.can_rerun_setup());
        assert!(app.sources().is_empty());

        // The store was never the problem, so the next request still lands.
        let preview = app
            .preview_source(&repository)
            .expect("preview source again");
        app.confirm_source(preview).expect("register the source");
        assert_eq!(app.sources().len(), 1);
    }
}
