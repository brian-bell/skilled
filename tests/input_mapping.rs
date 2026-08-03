use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use skilled::{Action, AppEnvironment, SetupStep, SkilledApp, View};

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
fn question_mark_opens_help_in_every_implemented_top_level_view() {
    use skilled::input::action_for_key;

    for view in [
        View::Setup(SetupStep::Welcome),
        View::Inventory,
        View::Sources,
        View::Settings,
    ] {
        assert_eq!(
            action_for_key(view, key(KeyCode::Char('?'))),
            Some(Action::OpenHelp),
            "view {view:?}"
        );
    }
}

#[test]
fn help_owns_input_until_escape_closes_it() {
    use skilled::input::action_for_app_key;

    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    app.update(Action::OpenHelp);

    assert_eq!(
        action_for_app_key(&app, key(KeyCode::Esc)),
        Some(Action::CloseHelp)
    );
    for blocked in [
        KeyCode::Char('q'),
        KeyCode::Char('?'),
        KeyCode::Enter,
        KeyCode::Char('2'),
    ] {
        assert_eq!(
            action_for_app_key(&app, key(blocked)),
            None,
            "key {blocked:?}"
        );
    }
    assert_eq!(
        action_for_app_key(
            &app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
        ),
        Some(Action::Quit)
    );
}

#[test]
fn escape_closes_help_before_the_underlying_settings_dialog() {
    use skilled::input::action_for_app_key;

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
            .expect("setup effects");
    }
    app.update(Action::OpenSettings);
    app.update(Action::OpenHelp);

    let close_help = action_for_app_key(&app, key(KeyCode::Esc)).expect("close help action");
    app.update(close_help);

    assert_eq!(app.view(), View::Settings);
    assert_eq!(app.help_context(), None);
    assert_eq!(
        action_for_app_key(&app, key(KeyCode::Esc)),
        Some(Action::Back)
    );
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
    assert_eq!(
        action_for_key(View::Inventory, repeat(KeyCode::Char('?'))),
        None
    );
}

#[test]
fn source_path_entry_treats_printable_keys_as_text_and_keeps_ctrl_c_as_quit() {
    use skilled::input::action_for_app_key;

    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    for _ in 0..7 {
        let update = app.update(Action::Continue);
        app.perform_effects(update.effects()).unwrap();
    }
    app.update(Action::OpenSources);
    app.update(Action::BeginAddSource);

    assert_eq!(
        action_for_app_key(&app, key(KeyCode::Char('q'))),
        Some(Action::AppendSourcePath('q'))
    );
    assert_eq!(
        action_for_app_key(&app, key(KeyCode::Char('?'))),
        Some(Action::AppendSourcePath('?'))
    );
    assert_eq!(
        action_for_app_key(&app, key(KeyCode::Enter)),
        Some(Action::SubmitSourcePath)
    );
    assert_eq!(
        action_for_app_key(
            &app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
        ),
        Some(Action::Quit)
    );
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn repeat(code: KeyCode) -> KeyEvent {
    KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Repeat)
}
