use skilled::{Action, AgentKind, AppEnvironment, SetupStep, SkilledApp, UpdateOutcome, View};

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
        app.update(Action::Continue).expect("advance to detection"),
        UpdateOutcome::Continue
    );
    assert_eq!(app.view(), View::Setup(SetupStep::DetectAgents));

    app.update(Action::MoveSelection(1)).expect("focus Codex");
    app.update(Action::ToggleSelection).expect("toggle Codex");

    assert!(app.agent(AgentKind::ClaudeCode).selected());
    assert!(!app.agent(AgentKind::Codex).selected());
    assert!(app.agent(AgentKind::OpenCode).selected());
}
