#[cfg(unix)]
mod unix {
    use std::{fs, os::unix::fs::PermissionsExt};

    use skilled::{AgentKind, AppEnvironment, SkilledApp};

    #[test]
    fn detection_observes_agent_executables_without_invoking_them() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let home = temporary.path().join("home");
        let bin = temporary.path().join("bin");
        let sentinel = temporary.path().join("agent-was-invoked");
        fs::create_dir_all(&bin).expect("create fake executable directory");

        for executable in ["claude", "codex", "opencode"] {
            let path = bin.join(executable);
            fs::write(
                &path,
                format!("#!/bin/sh\ntouch '{}'\n", sentinel.display()),
            )
            .expect("write fake executable");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
                .expect("make fake executable runnable");
        }

        let app = SkilledApp::open(AppEnvironment::new(
            &home,
            temporary.path().join("data"),
            bin.as_os_str(),
        ))
        .expect("open application");

        for agent in [AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::OpenCode] {
            let detection = app.agent(agent);
            assert!(detection.selected(), "{agent:?} should default to selected");
            assert!(
                detection.executable_path().is_some(),
                "{agent:?} should be detected"
            );
        }
        assert!(!sentinel.exists(), "detection must never execute an agent");
    }
}
