use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::{Action, AgentKind, SetupStep, SkilledApp, View};

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
    action_for_key(app.view(), key)
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
                _ => None,
            },
            View::Sources => match key.code {
                KeyCode::Char('1') => Some(Action::OpenInventory),
                KeyCode::Char('a') => Some(Action::BeginAddSource),
                KeyCode::Tab => Some(Action::MoveSourcesPane(1)),
                KeyCode::BackTab => Some(Action::MoveSourcesPane(-1)),
                KeyCode::Enter => Some(Action::AdvanceSourcesPane),
                KeyCode::Up | KeyCode::Char('k') => Some(Action::MoveSourcesSelection(-1)),
                KeyCode::Down | KeyCode::Char('j') => Some(Action::MoveSourcesSelection(1)),
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
