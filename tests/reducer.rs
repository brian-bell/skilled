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
