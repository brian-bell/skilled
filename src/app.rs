use crate::{
    AgentDetection, AgentKind, AppEnvironment, Result,
    agents::{detect_agents, detection_at},
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
    Settings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Continue,
    Back,
    MoveSelection(i8),
    ToggleSelection,
    OpenSettings,
    RerunSetup,
    Quit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateOutcome {
    Continue,
    Quit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    PersistSetup { agent_selections: [bool; 3] },
    ResetSetup,
    RedetectAgents { agent_selections: [bool; 3] },
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
        Ok(Self {
            view,
            store,
            environment,
            agents,
            focused_agent: 0,
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

    pub fn update(&mut self, action: Action) -> UpdateResult {
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
            Action::OpenSettings => {
                self.open_settings();
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
        match self.view {
            View::Setup(step) => {
                if let Some(previous) = step.previous() {
                    self.view = View::Setup(previous);
                }
            }
            View::Settings => self.view = View::Inventory,
            View::Inventory => {}
        }
        UpdateResult::continuing(Vec::new())
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
}
