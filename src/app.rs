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

pub struct SkilledApp {
    view: View,
    store: Store,
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

    pub fn update(&mut self, action: Action) -> Result<UpdateOutcome> {
        match action {
            Action::Continue => self.advance_setup()?,
            Action::Back => return Ok(self.back()),
            Action::MoveSelection(delta) => self.move_selection(delta),
            Action::ToggleSelection => self.toggle_selection(),
            Action::OpenSettings => self.open_settings(),
            Action::RerunSetup => self.rerun_setup()?,
            Action::Quit => return Ok(UpdateOutcome::Quit),
        }
        Ok(UpdateOutcome::Continue)
    }

    pub fn open_settings(&mut self) {
        if self.view == View::Inventory {
            self.view = View::Settings;
        }
    }

    pub fn rerun_setup(&mut self) -> Result<()> {
        if self.view == View::Settings {
            self.store.set_setup_complete(false)?;
            self.view = View::Setup(SetupStep::Welcome);
        }
        Ok(())
    }

    fn back(&mut self) -> UpdateOutcome {
        match self.view {
            View::Setup(step) => match step.previous() {
                Some(previous) => self.view = View::Setup(previous),
                None => return UpdateOutcome::Quit,
            },
            View::Settings => self.view = View::Inventory,
            View::Inventory => {}
        }
        UpdateOutcome::Continue
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

    pub fn advance_setup(&mut self) -> Result<()> {
        let View::Setup(step) = self.view else {
            return Ok(());
        };

        self.view = match step.next() {
            Some(next) => View::Setup(next),
            None => {
                self.store
                    .set_agent_selections(self.agents.each_ref().map(|agent| agent.selected()))?;
                self.store.set_setup_complete(true)?;
                View::Inventory
            }
        };
        Ok(())
    }
}
