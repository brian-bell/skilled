use std::path::{Path, PathBuf};

use crate::{
    AgentDetection, AgentKind, AppEnvironment, Result,
    agents::{detect_agents, detection_at},
    inventory::{InventoryRow, InventorySnapshot, scan_installations},
    source::{
        CatalogProposal, RegisteredSource, SkillCandidate, SourcePreview, preview_local_source,
        revalidate_source_preview,
    },
    store::Store,
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
    Settings,
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
    MoveInventoryPane(i8),
    AdvanceInventoryPane,
    MoveInventorySelection(i8),
    BeginInventoryFilter,
    AppendInventoryFilter(char),
    DeleteInventoryFilterCharacter,
    SubmitInventoryFilter,
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
    PersistSetup { agent_selections: [bool; 3] },
    ResetSetup,
    RedetectAgents { agent_selections: [bool; 3] },
    InspectSource { path: PathBuf },
    RegisterSource { preview: SourcePreview },
    ScanInstallations,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateResult {
    outcome: UpdateOutcome,
    effects: Vec<Effect>,
}

impl UpdateResult {
    fn continuing(effects: Vec<Effect>) -> Self {
        Self {
            outcome: UpdateOutcome::Continue,
            effects,
        }
    }

    fn quit() -> Self {
        Self {
            outcome: UpdateOutcome::Quit,
            effects: Vec::new(),
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

pub struct SkilledApp {
    view: View,
    store: Store,
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
    inventory: InventorySnapshot,
    /// Indices into `inventory.rows()` that the filter admits.
    ///
    /// Recomputed whenever the snapshot or the query changes rather than on
    /// every frame, because rendering and the key-hint bar both ask for it.
    filtered_installations: Vec<usize>,
    inventory_pane: InventoryPane,
    focused_installation: usize,
    inventory_filter: String,
    inventory_filter_active: bool,
    help_context: Option<View>,
}

impl SkilledApp {
    pub fn open(environment: AppEnvironment) -> Result<Self> {
        let store = Store::open(&environment.data_dir)?;
        let view = if store.setup_complete()? {
            View::Inventory
        } else {
            View::Setup(SetupStep::Welcome)
        };
        let mut agents = detect_agents(&environment);
        if let Some(selections) = store.agent_selections()? {
            for (agent, selected) in agents.iter_mut().zip(selections) {
                agent.set_selected(selected);
            }
        }
        let sources = store.registered_sources()?;
        // Setup reads the installation roots at its own step, after the user
        // has chosen which agents Skilled should configure. Reading them
        // before that would look at roots the user may be about to deselect.
        // Once setup is complete the selections are known, so opening
        // straight into the Inventory opens onto a real scan.
        let inventory = if view == View::Inventory {
            scan_installations(&agents, &sources)
        } else {
            InventorySnapshot::not_scanned(&agents)
        };
        let mut app = Self {
            view,
            store,
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
            inventory,
            filtered_installations: Vec::new(),
            inventory_pane: InventoryPane::Skills,
            focused_installation: 0,
            inventory_filter: String::new(),
            inventory_filter_active: false,
            help_context: None,
        };
        app.refilter_installations();
        Ok(app)
    }

    pub fn view(&self) -> View {
        self.view
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

    pub fn inventory_pane(&self) -> InventoryPane {
        self.inventory_pane
    }

    pub fn focused_installation(&self) -> usize {
        self.focused_installation
    }

    pub fn inventory_filter(&self) -> &str {
        &self.inventory_filter
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
        self.store.register_source(&preview)?;
        self.sources = self.store.registered_sources()?;
        self.focus_registered_source(preview.inspected().git_top_level());
        self.rescan_installations();
        Ok(())
    }

    pub fn update(&mut self, action: Action) -> UpdateResult {
        if self.help_context.is_some() {
            return match action {
                Action::CloseHelp => {
                    self.help_context = None;
                    UpdateResult::continuing(Vec::new())
                }
                Action::Quit => UpdateResult::quit(),
                _ => UpdateResult::continuing(Vec::new()),
            };
        }

        // The filter bar owns the keyboard while it is open, so a stray action
        // cannot navigate out from under a half-typed query. Quitting is the
        // one command no context may swallow.
        if self.inventory_filter_active {
            return match action {
                Action::Quit => UpdateResult::quit(),
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
                if self.view == View::Sources {
                    self.enter_inventory()
                } else {
                    Vec::new()
                }
            }
            Action::OpenSources => {
                if self.view == View::Inventory {
                    self.view = View::Sources;
                    self.sources_pane = SourcesPane::Repositories;
                }
                Vec::new()
            }
            Action::BeginAddSource => {
                if self.view == View::Sources
                    || self.view == View::Setup(SetupStep::DiscoverSources)
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
            Action::ConfirmPendingSource => self.register_pending_source(),
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
            Action::RerunSetup => self.rerun_setup(),
            Action::Quit => return UpdateResult::quit(),
        };
        UpdateResult::continuing(effects)
    }

    pub fn perform_effects(&mut self, effects: &[Effect]) -> Result<()> {
        for effect in effects {
            match effect {
                Effect::PersistSetup { agent_selections } => {
                    self.store.complete_setup(*agent_selections)?;
                }
                Effect::ResetSetup => self.store.set_setup_complete(false)?,
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
                    if let Err(error) = self.store.register_source(&preview) {
                        self.source_error = Some(error.to_string());
                        continue;
                    }
                    self.sources = self.store.registered_sources()?;
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
            }
        }
        Ok(())
    }

    /// Replace the inventory with a fresh read-only pass over the native roots.
    ///
    /// This is the only place installation scanning happens; the reducer stays
    /// free of filesystem work.
    fn rescan_installations(&mut self) {
        self.inventory = scan_installations(&self.agents, &self.sources);
        self.refilter_installations();
    }

    /// Recompute which rows the query admits, and keep the selection on one.
    ///
    /// A row matches when the query appears in its name, the label of its
    /// provenance or of any installation's own provenance, or the word naming
    /// its health, so the same box narrows by identity, provenance, or state.
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
                    || row.health().label().contains(needle.as_str())
            })
            .map(|(index, _)| index)
            .collect();
        let last = self.filtered_installations.len().saturating_sub(1);
        self.focused_installation = self.focused_installation.min(last);
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
        self.inventory = InventorySnapshot::not_scanned(&self.agents);
        // The gap snapshot holds no rows, so the only consistent filtered
        // list is empty whatever the query. Clearing it directly — rather
        // than refiltering — leaves the focused row alone: the scan that
        // lands refilters and re-clamps it against the fresh rows, so a row
        // that is still there keeps its selection.
        self.filtered_installations.clear();
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
            View::Sources => match self.sources_pane {
                SourcesPane::Details => self.sources_pane = SourcesPane::Variants,
                SourcesPane::Variants => self.sources_pane = SourcesPane::Repositories,
                SourcesPane::Repositories => {
                    return UpdateResult::continuing(self.enter_inventory());
                }
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
        let View::Setup(step) = self.view else {
            return Vec::new();
        };

        if step == SetupStep::ConfirmCatalogs && self.pending_source.is_some() {
            return self.register_pending_source();
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
