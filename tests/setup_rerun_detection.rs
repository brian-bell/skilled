#[cfg(unix)]
mod unix {
    use std::{fs, os::unix::fs::PermissionsExt};

    use skilled::{Action, AgentKind, AppEnvironment, SkilledApp};

    #[test]
    fn rerunning_setup_refreshes_detection_without_losing_selections_or_invoking_agents() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let home = temporary.path().join("home");
        let bin = temporary.path().join("bin");
        let sentinel = temporary.path().join("agent-was-invoked");
        fs::create_dir_all(&bin).expect("create executable directory");
        let environment = AppEnvironment::new(&home, temporary.path().join("data"), &bin);
        let mut app = SkilledApp::open(environment).expect("open application");

        dispatch(&mut app, Action::Continue);
        dispatch(&mut app, Action::MoveSelection(1));
        dispatch(&mut app, Action::ToggleSelection);
        for _ in 0..6 {
            dispatch(&mut app, Action::Continue);
        }
        assert!(!app.agent(AgentKind::Codex).selected());
        assert!(!app.agent(AgentKind::Codex).root_exists());
        assert!(app.agent(AgentKind::Codex).executable_path().is_none());

        fs::create_dir_all(home.join(".agents/skills")).expect("create Codex skill root");
        let executable = bin.join("codex");
        fs::write(
            &executable,
            format!("#!/bin/sh\ntouch '{}'\n", sentinel.display()),
        )
        .expect("write fake Codex executable");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
            .expect("make fake Codex executable runnable");

        dispatch(&mut app, Action::OpenSettings);
        dispatch(&mut app, Action::RerunSetup);

        let codex = app.agent(AgentKind::Codex);
        assert!(!codex.selected(), "rerun should preserve agent selection");
        assert!(codex.root_exists(), "rerun should refresh root detection");
        assert_eq!(codex.executable_path(), Some(executable.as_path()));
        assert!(!sentinel.exists(), "rerun must never execute an agent");
    }

    fn dispatch(app: &mut SkilledApp, action: Action) {
        let update = app.update(action);
        app.perform_effects(update.effects())
            .expect("perform effects");
    }
}
