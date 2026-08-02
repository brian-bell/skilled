use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::{Action, SetupStep, View};

pub fn action_for_key(view: View, key: KeyEvent) -> Option<Action> {
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return None;
    }
    let action = if (key.code == KeyCode::Char('c')
        && key.modifiers.contains(KeyModifiers::CONTROL))
        || (key.code == KeyCode::Char('q') && view != View::Settings)
    {
        Some(Action::Quit)
    } else {
        match view {
            View::Setup(step) => setup_action(step, key.code),
            View::Inventory => match key.code {
                KeyCode::Char('s') => Some(Action::OpenSettings),
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
        _ => None,
    }
}
