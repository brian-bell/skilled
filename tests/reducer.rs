use skilled::{
    Action, AgentKind, AppEnvironment, Effect, SetupStep, SkilledApp, UpdateOutcome, View,
};

#[test]
fn setup_actions_advance_and_change_the_focused_agent_selection() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");

    assert_eq!(
        app.update(Action::Continue).outcome(),
        UpdateOutcome::Continue
    );
    assert_eq!(app.view(), View::Setup(SetupStep::DetectAgents));

    app.update(Action::MoveSelection(1));
    app.update(Action::ToggleSelection);

    assert!(app.agent(AgentKind::ClaudeCode).selected());
    assert!(!app.agent(AgentKind::Codex).selected());
    assert!(app.agent(AgentKind::OpenCode).selected());
}

#[test]
fn finishing_setup_returns_a_persistence_effect_without_writing_metadata() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let environment = AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    );
    let mut app = SkilledApp::open(environment.clone()).expect("open application");
    for _ in 0..6 {
        app.update(Action::Continue);
    }

    let update = app.update(Action::Continue);

    assert_eq!(update.outcome(), UpdateOutcome::Continue);
    assert_eq!(
        update.effects(),
        [Effect::PersistSetup {
            agent_selections: [true, true, true]
        }]
    );
    drop(app);
    assert_eq!(
        SkilledApp::open(environment)
            .expect("reopen without executing effect")
            .view(),
        View::Setup(SetupStep::Welcome)
    );
}

#[test]
fn back_is_a_no_op_on_the_first_setup_step() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");

    let update = app.update(Action::Back);

    assert_eq!(update.outcome(), UpdateOutcome::Continue);
    assert!(update.effects().is_empty());
    assert_eq!(app.view(), View::Setup(SetupStep::Welcome));
}

#[test]
fn help_captures_and_protects_every_implemented_top_level_context() {
    let setup_directory = tempfile::tempdir().expect("temporary application directory");
    let mut setup = app_in(&setup_directory);
    setup.update(Action::Continue);
    assert_help_blocks(&mut setup, Action::Continue);

    let inventory_directory = tempfile::tempdir().expect("temporary application directory");
    let mut inventory = app_in(&inventory_directory);
    finish_setup(&mut inventory);
    assert_help_blocks(&mut inventory, Action::OpenSources);

    let sources_directory = tempfile::tempdir().expect("temporary application directory");
    let mut sources = app_in(&sources_directory);
    finish_setup(&mut sources);
    sources.update(Action::OpenSources);
    assert_help_blocks(&mut sources, Action::Back);

    let settings_directory = tempfile::tempdir().expect("temporary application directory");
    let mut settings = app_in(&settings_directory);
    finish_setup(&mut settings);
    settings.update(Action::OpenSettings);
    assert_help_blocks(&mut settings, Action::RerunSetup);
}

#[test]
fn help_does_not_open_over_source_path_entry() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    for _ in 0..3 {
        app.update(Action::Continue);
    }
    app.update(Action::BeginAddSource);
    assert!(app.source_path_input_active());

    app.update(Action::OpenHelp);

    assert_eq!(app.help_context(), None);
    assert!(app.source_path_input_active());
}

#[test]
fn sources_add_flow_collects_a_path_before_requesting_inspection() {
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
    for character in "/tmp/source ".chars() {
        app.update(Action::AppendSourcePath(character));
    }
    let update = app.update(Action::SubmitSourcePath);

    assert_eq!(app.view(), View::Sources);
    assert!(app.source_path_input_active());
    assert_eq!(
        update.effects(),
        [Effect::InspectSource {
            path: "/tmp/source ".into()
        }]
    );
}

fn app_in(directory: &tempfile::TempDir) -> SkilledApp {
    SkilledApp::open(AppEnvironment::new(
        directory.path().join("home"),
        directory.path().join("data"),
        "",
    ))
    .expect("open application")
}

fn finish_setup(app: &mut SkilledApp) {
    for _ in 0..7 {
        let update = app.update(Action::Continue);
        app.perform_effects(update.effects())
            .expect("setup effects");
    }
}

fn assert_help_blocks(app: &mut SkilledApp, blocked_action: Action) {
    let original_view = app.view();

    let opened = app.update(Action::OpenHelp);
    assert_eq!(opened.outcome(), UpdateOutcome::Continue);
    assert!(opened.effects().is_empty());
    assert_eq!(app.help_context(), Some(original_view));
    assert_eq!(app.view(), original_view);

    let blocked = app.update(blocked_action);
    assert_eq!(blocked.outcome(), UpdateOutcome::Continue);
    assert!(blocked.effects().is_empty());
    assert_eq!(app.help_context(), Some(original_view));
    assert_eq!(app.view(), original_view);

    let closed = app.update(Action::CloseHelp);
    assert_eq!(closed.outcome(), UpdateOutcome::Continue);
    assert!(closed.effects().is_empty());
    assert_eq!(app.help_context(), None);
    assert_eq!(app.view(), original_view);
}
