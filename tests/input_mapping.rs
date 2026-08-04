use std::{fs, path::Path, process::Command};

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
fn sources_tab_and_backtab_move_region_focus_in_opposite_directions() {
    use skilled::input::action_for_key;

    assert_eq!(
        action_for_key(View::Sources, key(KeyCode::Tab)),
        Some(Action::MoveSourcesPane(1))
    );
    assert_eq!(
        action_for_key(View::Sources, key(KeyCode::BackTab)),
        Some(Action::MoveSourcesPane(-1))
    );
}

#[test]
fn sources_enter_advances_and_escape_backs_through_the_region_hierarchy() {
    use skilled::input::action_for_key;

    assert_eq!(
        action_for_key(View::Sources, key(KeyCode::Enter)),
        Some(Action::AdvanceSourcesPane)
    );
    assert_eq!(
        action_for_key(View::Sources, key(KeyCode::Esc)),
        Some(Action::Back)
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
fn repeated_sources_hierarchy_keys_do_not_skip_regions() {
    use skilled::input::action_for_key;

    for code in [KeyCode::Tab, KeyCode::BackTab, KeyCode::Enter, KeyCode::Esc] {
        assert_eq!(
            action_for_key(View::Sources, repeat(code)),
            None,
            "{code:?}"
        );
    }
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

#[test]
fn pending_catalog_confirmation_precedes_sources_region_navigation() {
    use skilled::input::action_for_app_key;

    let temporary = tempfile::tempdir().expect("temporary application directory");
    let repository = temporary.path().join("source");
    fs::create_dir_all(repository.join("skills/portable")).expect("create skill fixture");
    fs::write(
        repository.join("skills/portable/SKILL.md"),
        "---\nname: portable\ndescription: fixture\n---\n# Portable\n",
    )
    .expect("write skill fixture");
    initialize_repository(&repository);
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
    app.update(Action::OpenSources);
    app.update(Action::BeginAddSource);
    for character in repository.to_string_lossy().chars() {
        app.update(Action::AppendSourcePath(character));
    }
    let update = app.update(Action::SubmitSourcePath);
    app.perform_effects(update.effects())
        .expect("inspect source");
    assert!(app.pending_source().is_some());

    assert_eq!(
        action_for_app_key(&app, key(KeyCode::Enter)),
        Some(Action::ConfirmPendingSource)
    );
    assert_eq!(
        action_for_app_key(&app, key(KeyCode::Esc)),
        Some(Action::CancelSourceFlow)
    );
    assert_eq!(action_for_app_key(&app, key(KeyCode::Tab)), None);
    assert_eq!(action_for_app_key(&app, key(KeyCode::BackTab)), None);
    assert_eq!(action_for_app_key(&app, repeat(KeyCode::Enter)), None);
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn repeat(code: KeyCode) -> KeyEvent {
    KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Repeat)
}

fn initialize_repository(repository: &Path) {
    git(repository, &["init", "-b", "main"]);
    git(repository, &["config", "user.name", "Skilled Test"]);
    git(
        repository,
        &["config", "user.email", "skilled@example.test"],
    );
    git(repository, &["add", "."]);
    git(repository, &["commit", "-m", "fixture"]);
}

fn git(repository: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .expect("run Git fixture command");
    assert!(output.status.success());
}
