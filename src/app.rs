use std::path::{Path, PathBuf};

use crate::{
    AgentDetection, AgentKind, AppEnvironment, Result,
    agents::{detect_agents, detection_at},
    source::{RegisteredSource, SourcePreview, preview_local_source, revalidate_source_preview},
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
        Ok(Self {
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
            help_context: None,
        })
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

    pub fn help_context(&self) -> Option<View> {
        self.help_context
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

        let effects = match action {
            Action::Continue => self.advance_setup().into_iter().collect(),
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
                if !self.source_path_input_active && self.pending_source.is_none() {
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
                if matches!(self.view, View::Inventory | View::Sources) {
                    self.view = View::Inventory;
                }
                Vec::new()
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
                if let Some(preview) = &self.pending_source
                    && !preview.catalogs().is_empty()
                {
                    self.focused_catalog = (self.focused_catalog as i16 + i16::from(delta))
                        .rem_euclid(preview.catalogs().len() as i16)
                        as usize;
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
                    self.sources_pane = match (index + i16::from(delta)).rem_euclid(3) {
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
                }
            }
        }
        Ok(())
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
            View::Settings => self.view = View::Inventory,
            View::Sources => match self.sources_pane {
                SourcesPane::Details => self.sources_pane = SourcesPane::Variants,
                SourcesPane::Variants => self.sources_pane = SourcesPane::Repositories,
                SourcesPane::Repositories => self.view = View::Inventory,
            },
            View::Inventory => {}
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
        self.focused_agent = (self.focused_agent as i16 + i16::from(delta))
            .rem_euclid(self.agents.len() as i16) as usize;
    }

    fn toggle_selection(&mut self) {
        if self.view == View::Setup(SetupStep::DetectAgents) {
            self.agents[self.focused_agent].toggle_selected();
        }
    }

    fn advance_setup(&mut self) -> Option<Effect> {
        let View::Setup(step) = self.view else {
            return None;
        };

        if step == SetupStep::ConfirmCatalogs && self.pending_source.is_some() {
            return self.register_pending_source().into_iter().next();
        }

        match step.next() {
            Some(next) => {
                self.view = View::Setup(next);
                None
            }
            None => {
                self.view = View::Inventory;
                Some(Effect::PersistSetup {
                    agent_selections: self.agents.each_ref().map(|agent| agent.selected()),
                })
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
                let next = (self.focused_source as i16 + i16::from(delta))
                    .rem_euclid(self.sources.len() as i16) as usize;
                if next != self.focused_source {
                    self.focused_source = next;
                    self.focused_variant = 0;
                }
            }
            SourcesPane::Variants => {
                let count = self
                    .selected_source()
                    .into_iter()
                    .flat_map(RegisteredSource::catalogs)
                    .map(|catalog| catalog.candidates().len())
                    .sum::<usize>();
                if count > 0 {
                    self.focused_variant = (self.focused_variant as i16 + i16::from(delta))
                        .rem_euclid(count as i16)
                        as usize;
                }
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
