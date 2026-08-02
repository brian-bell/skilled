use skilled::{Action, AgentKind, AppEnvironment, SetupStep, SkilledApp, View};

#[test]
fn completing_setup_makes_inventory_the_next_startup_view() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let environment = AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    );

    let mut first_launch = SkilledApp::open(environment.clone()).expect("first launch");
    assert_eq!(first_launch.view(), View::Setup(SetupStep::Welcome));

    for _ in 0..7 {
        first_launch.advance_setup().expect("advance setup");
    }
    assert_eq!(first_launch.view(), View::Inventory);
    drop(first_launch);

    let second_launch = SkilledApp::open(environment).expect("second launch");
    assert_eq!(second_launch.view(), View::Inventory);
}

#[test]
fn settings_can_rerun_setup_and_persist_that_choice() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let environment = AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    );
    let mut app = SkilledApp::open(environment.clone()).expect("open application");
    for _ in 0..7 {
        app.advance_setup().expect("complete setup");
    }

    app.open_settings();
    assert_eq!(app.view(), View::Settings);
    app.rerun_setup().expect("rerun setup");
    assert_eq!(app.view(), View::Setup(SetupStep::Welcome));
    drop(app);

    let next_launch = SkilledApp::open(environment).expect("reopen application");
    assert_eq!(next_launch.view(), View::Setup(SetupStep::Welcome));
}

#[test]
fn setup_persists_the_configured_agent_selection() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let environment = AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    );
    let mut app = SkilledApp::open(environment.clone()).expect("open application");

    app.update(Action::Continue).expect("open agent selection");
    app.update(Action::MoveSelection(1)).expect("focus Codex");
    app.update(Action::ToggleSelection).expect("disable Codex");
    for _ in 0..6 {
        app.update(Action::Continue).expect("complete setup");
    }
    drop(app);

    let reopened = SkilledApp::open(environment).expect("reopen application");
    assert!(reopened.agent(AgentKind::ClaudeCode).selected());
    assert!(!reopened.agent(AgentKind::Codex).selected());
    assert!(reopened.agent(AgentKind::OpenCode).selected());
}
