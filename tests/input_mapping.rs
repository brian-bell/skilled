use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use skilled::{Action, SetupStep, View};

#[test]
fn keys_map_to_contextual_actions_without_mutating_application_state() {
    use skilled::input::action_for_key;

    assert_eq!(
        action_for_key(View::Setup(SetupStep::Welcome), key(KeyCode::Enter)),
        Some(Action::Continue)
    );
    assert_eq!(
        action_for_key(View::Setup(SetupStep::DetectAgents), key(KeyCode::Down)),
        Some(Action::MoveSelection(1))
    );
    assert_eq!(
        action_for_key(
            View::Setup(SetupStep::DetectAgents),
            key(KeyCode::Char(' '))
        ),
        Some(Action::ToggleSelection)
    );
    assert_eq!(
        action_for_key(View::Inventory, key(KeyCode::Char('s'))),
        Some(Action::OpenSettings)
    );
    assert_eq!(
        action_for_key(View::Settings, key(KeyCode::Enter)),
        Some(Action::RerunSetup)
    );
    assert_eq!(
        action_for_key(View::Settings, key(KeyCode::Esc)),
        Some(Action::Back)
    );
    assert_eq!(
        action_for_key(View::Inventory, key(KeyCode::Char('q'))),
        Some(Action::Quit)
    );
    assert_eq!(
        action_for_key(
            View::Inventory,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
        ),
        Some(Action::Quit)
    );
}

#[test]
fn actions_remain_copyable_values() {
    fn assert_copy<T: Copy>() {}

    assert_copy::<Action>();
}

#[test]
fn repeated_keys_only_move_the_agent_selection() {
    use skilled::input::action_for_key;

    assert_eq!(
        action_for_key(View::Setup(SetupStep::DetectAgents), repeat(KeyCode::Down)),
        Some(Action::MoveSelection(1))
    );
    assert_eq!(
        action_for_key(View::Setup(SetupStep::Welcome), repeat(KeyCode::Enter)),
        None
    );
    assert_eq!(
        action_for_key(
            View::Setup(SetupStep::DetectAgents),
            repeat(KeyCode::Char(' '))
        ),
        None
    );
    assert_eq!(action_for_key(View::Settings, repeat(KeyCode::Enter)), None);
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn repeat(code: KeyCode) -> KeyEvent {
    KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Repeat)
}
