use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::{Action, AgentKind, DoctorPane, InventoryPane, SetupStep, SkilledApp, View};

pub fn action_for_app_key(app: &SkilledApp, key: KeyEvent) -> Option<Action> {
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return None;
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return (key.kind == KeyEventKind::Press).then_some(Action::Quit);
    }
    if app.help_context().is_some() {
        return (key.kind == KeyEventKind::Press && key.code == KeyCode::Esc)
            .then_some(Action::CloseHelp);
    }
    if app.source_path_input_active() {
        let action = match key.code {
            KeyCode::Enter => Some(Action::SubmitSourcePath),
            KeyCode::Esc => Some(Action::CancelSourceFlow),
            KeyCode::Backspace => Some(Action::DeleteSourcePathCharacter),
            KeyCode::Char(character) => Some(Action::AppendSourcePath(character)),
            _ => None,
        };
        return match (key.kind, action) {
            (KeyEventKind::Repeat, Some(Action::DeleteSourcePathCharacter)) => action,
            (KeyEventKind::Repeat, Some(Action::AppendSourcePath(_))) => action,
            (KeyEventKind::Repeat, _) => None,
            _ => action,
        };
    }
    // The filter bar is a text field, so every printable key is a character
    // rather than the route or command it would be elsewhere in the Inventory.
    if app.inventory_filter_active() {
        let action = match key.code {
            KeyCode::Enter => Some(Action::SubmitInventoryFilter),
            KeyCode::Esc => Some(Action::Back),
            KeyCode::Backspace => Some(Action::DeleteInventoryFilterCharacter),
            KeyCode::Char(character) => Some(Action::AppendInventoryFilter(character)),
            _ => None,
        };
        return match (key.kind, action) {
            (KeyEventKind::Repeat, Some(Action::DeleteInventoryFilterCharacter)) => action,
            (KeyEventKind::Repeat, Some(Action::AppendInventoryFilter(_))) => action,
            (KeyEventKind::Repeat, _) => None,
            _ => action,
        };
    }
    if app.pending_source().is_some() {
        let action = match key.code {
            KeyCode::Enter => Some(Action::ConfirmPendingSource),
            KeyCode::Esc => Some(Action::CancelSourceFlow),
            KeyCode::Up | KeyCode::Char('k') => Some(Action::MoveCatalogSelection(-1)),
            KeyCode::Down | KeyCode::Char('j') => Some(Action::MoveCatalogSelection(1)),
            KeyCode::Char(' ') => Some(Action::ToggleCatalogIncluded),
            KeyCode::Char('c') => Some(Action::ToggleCatalogClassification),
            KeyCode::Char('1') => Some(Action::ToggleCatalogCompatibility(AgentKind::ClaudeCode)),
            KeyCode::Char('2') => Some(Action::ToggleCatalogCompatibility(AgentKind::Codex)),
            KeyCode::Char('3') => Some(Action::ToggleCatalogCompatibility(AgentKind::OpenCode)),
            _ => None,
        };
        return match (key.kind, action) {
            (KeyEventKind::Repeat, Some(Action::MoveCatalogSelection(_))) => action,
            (KeyEventKind::Repeat, _) => None,
            _ => action,
        };
    }
    // The keys that move a workspace's selection move its detail region's
    // window once that region has focus. The translation happens here rather than in
    // `action_for_key` because only this side sees which region is focused —
    // and it happens after that call so the held-key allowance the movement
    // keys already have carries over to scrolling, which is where a user is
    // most likely to hold one down.
    match action_for_key(app.view(), key) {
        Some(Action::MoveInventorySelection(delta))
            if app.view() == View::Inventory && app.inventory_pane() == InventoryPane::Details =>
        {
            Some(Action::ScrollDetail(delta))
        }
        Some(Action::MoveDoctorSelection(delta))
            if app.view() == View::Doctor && app.doctor_pane() == DoctorPane::Details =>
        {
            Some(Action::ScrollDetail(delta))
        }
        action => action,
    }
}

pub fn action_for_key(view: View, key: KeyEvent) -> Option<Action> {
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return None;
    }
    let action = if (key.code == KeyCode::Char('c')
        && key.modifiers.contains(KeyModifiers::CONTROL))
        || (key.code == KeyCode::Char('q') && view != View::Settings)
    {
        Some(Action::Quit)
    } else if key.code == KeyCode::Char('?') {
        Some(Action::OpenHelp)
    } else {
        match view {
            View::Setup(step) => setup_action(step, key.code),
            View::Inventory => match key.code {
                KeyCode::Char('s') => Some(Action::OpenSettings),
                KeyCode::Char('2') => Some(Action::OpenSources),
                KeyCode::Char('4') => Some(Action::OpenDoctor),
                KeyCode::Tab => Some(Action::MoveInventoryPane(1)),
                KeyCode::BackTab => Some(Action::MoveInventoryPane(-1)),
                KeyCode::Enter => Some(Action::AdvanceInventoryPane),
                KeyCode::Up | KeyCode::Char('k') => Some(Action::MoveInventorySelection(-1)),
                KeyCode::Down | KeyCode::Char('j') => Some(Action::MoveInventorySelection(1)),
                KeyCode::Char('/') => Some(Action::BeginInventoryFilter),
                KeyCode::Esc => Some(Action::Back),
                _ => None,
            },
            View::Sources => match key.code {
                KeyCode::Char('1') => Some(Action::OpenInventory),
                KeyCode::Char('4') => Some(Action::OpenDoctor),
                KeyCode::Char('a') => Some(Action::BeginAddSource),
                KeyCode::Tab => Some(Action::MoveSourcesPane(1)),
                KeyCode::BackTab => Some(Action::MoveSourcesPane(-1)),
                KeyCode::Enter => Some(Action::AdvanceSourcesPane),
                KeyCode::Up | KeyCode::Char('k') => Some(Action::MoveSourcesSelection(-1)),
                KeyCode::Down | KeyCode::Char('j') => Some(Action::MoveSourcesSelection(1)),
                KeyCode::Esc => Some(Action::Back),
                _ => None,
            },
            View::Doctor => match key.code {
                KeyCode::Char('1') => Some(Action::OpenInventory),
                KeyCode::Char('2') => Some(Action::OpenSources),
                KeyCode::Tab => Some(Action::MoveDoctorPane(1)),
                KeyCode::BackTab => Some(Action::MoveDoctorPane(-1)),
                KeyCode::Enter => Some(Action::AdvanceDoctorPane),
                KeyCode::Up | KeyCode::Char('k') => Some(Action::MoveDoctorSelection(-1)),
                KeyCode::Down | KeyCode::Char('j') => Some(Action::MoveDoctorSelection(1)),
                KeyCode::Esc => Some(Action::Back),
                _ => None,
            },
            View::Settings => match key.code {
                KeyCode::Enter => Some(Action::RerunSetup),
                KeyCode::Esc => Some(Action::Back),
                _ => None,
            },
        }
    };

    match (key.kind, action) {
        (KeyEventKind::Repeat, Some(Action::MoveSelection(_))) => action,
        (KeyEventKind::Repeat, Some(Action::MoveInventorySelection(_))) => action,
        (KeyEventKind::Repeat, Some(Action::MoveDoctorSelection(_))) => action,
        (KeyEventKind::Repeat, _) => None,
        _ => action,
    }
}

fn setup_action(step: SetupStep, code: KeyCode) -> Option<Action> {
    match code {
        KeyCode::Enter => Some(Action::Continue),
        KeyCode::Esc => Some(Action::Back),
        KeyCode::Up | KeyCode::Char('k') if step == SetupStep::DetectAgents => {
            Some(Action::MoveSelection(-1))
        }
        KeyCode::Down | KeyCode::Char('j') if step == SetupStep::DetectAgents => {
            Some(Action::MoveSelection(1))
        }
        KeyCode::Char(' ') if step == SetupStep::DetectAgents => Some(Action::ToggleSelection),
        KeyCode::Char('a') if step == SetupStep::DiscoverSources => Some(Action::BeginAddSource),
        _ => None,
    }
}
