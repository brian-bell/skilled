#![cfg(windows)]

use std::{collections::BTreeMap, path::Path, process::Command};

use skilled::{
    AppEnvironment, SkilledApp,
    updates::{RepositoryUpdateVerdict, classify_repository_update, probe_repository_update},
};

const GUARDS: &[(&str, &str)] = &[
    ("GIT_ALLOW_PROTOCOL", "ssh"),
    ("GIT_ASKPASS", "ignored-askpass"),
    ("GIT_NO_LAZY_FETCH", "1"),
    ("GIT_OPTIONAL_LOCKS", "0"),
    ("GIT_SSH_COMMAND", "selected-before-child-spawn"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("SSH_ASKPASS_REQUIRE", "never"),
];

const PROTECTED_GUARDS: &[&str] = &[
    "GIT_ALLOW_PROTOCOL",
    "GIT_ASKPASS",
    "GIT_DIR",
    "GIT_LITERAL_PATHSPECS",
    "GIT_NO_LAZY_FETCH",
    "GIT_OPTIONAL_LOCKS",
    "GIT_PROTOCOL_FROM_USER",
    "GIT_SSH_COMMAND",
    "GIT_TERMINAL_PROMPT",
    "GIT_WORK_TREE",
    "SSH_ASKPASS_REQUIRE",
];

struct GitFixture {
    _temporary: tempfile::TempDir,
    home: std::path::PathBuf,
    app: SkilledApp,
    clone: std::path::PathBuf,
}

impl GitFixture {
    fn command(&self, repository: &Path) -> Command {
        isolated_git(&self.home, repository)
    }

    fn git(&self, repository: &Path, arguments: &[&str]) {
        let output = self
            .command(repository)
            .args(arguments)
            .output()
            .expect("run Git fixture command");
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn isolated_git(home: &Path, repository: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repository)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_CONFIG_GLOBAL");
    command
}

fn fixture() -> GitFixture {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let home = temporary.path().join("home");
    let remote = temporary.path().join("remote.git");
    let seed = temporary.path().join("seed");
    let clone = temporary.path().join("clone");
    let new_git = |arguments: &[&str], path: &Path| {
        let output = Command::new("git")
            .args(arguments)
            .arg(path)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_CONFIG_GLOBAL")
            .output()
            .expect("create fixture repository");
        assert!(output.status.success(), "{:?}", output);
    };
    new_git(&["init", "--bare"], &remote);
    new_git(&["init", "-b", "main"], &seed);
    std::fs::write(seed.join("README.md"), "fixture\n").expect("write fixture");
    let git = |repository: &Path, arguments: &[&str]| {
        let output = isolated_git(&home, repository)
            .args(arguments)
            .output()
            .expect("run Git fixture command");
        assert!(output.status.success(), "git {arguments:?}: {output:?}");
    };
    git(&seed, &["add", "."]);
    git(
        &seed,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-m",
            "initial",
        ],
    );
    git(
        &seed,
        &[
            "remote",
            "add",
            "origin",
            remote.to_str().expect("remote path"),
        ],
    );
    git(&seed, &["push", "-u", "origin", "main"]);
    let output = Command::new("git")
        .args(["clone", "--branch", "main"])
        .arg(&remote)
        .arg(&clone)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_CONFIG_GLOBAL")
        .output()
        .expect("clone fixture");
    assert!(output.status.success(), "{:?}", output);

    let environment = AppEnvironment::new(&home, temporary.path().join("data"), "");
    let mut app = SkilledApp::open(environment).expect("open app");
    let preview = app.preview_source(&clone).expect("preview clone");
    app.confirm_source(preview).expect("register clone");
    GitFixture {
        _temporary: temporary,
        home,
        app,
        clone,
    }
}

fn recorded_environment(fixture: &GitFixture, unset: Option<&str>) -> BTreeMap<String, String> {
    let marker = fixture._temporary.path().join("environment.txt");
    let script = fixture._temporary.path().join("record-environment.cmd");
    if marker.exists() {
        std::fs::remove_file(&marker).expect("remove prior recorder output");
    }
    let script_body = format!(
        "@echo off\r\n(\r\n{}\r\n) > \"%SKILLED_GUARD_MARKER%\"\r\nexit /b 1\r\n",
        GUARDS
            .iter()
            .map(|(name, _)| format!("echo {name}=%{name}%"))
            .collect::<Vec<_>>()
            .join("\r\n")
    );
    std::fs::write(&script, script_body).expect("write SSH recorder");
    let script_command = format!("\"{}\"", script.display().to_string().replace('\\', "/"));
    let mut command = fixture.command(&fixture.clone);
    command
        .arg("-c")
        .arg(format!("core.unsetenvvars={}", unset.unwrap_or("")))
        .args(["ls-remote", "ssh://example.invalid/repository.git"])
        .env("GIT_SSH_COMMAND", script_command)
        .env("GIT_SSH_VARIANT", "ssh")
        .env("SKILLED_GUARD_MARKER", &marker);
    for (name, value) in GUARDS {
        if *name != "GIT_SSH_COMMAND" {
            command.env(name, value);
        }
    }
    let output = command.output().expect("run SSH child fixture");
    assert!(
        !output.status.success(),
        "recorder must stop the SSH attempt"
    );
    std::fs::read_to_string(&marker)
        .expect("Git started the SSH child")
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

/// `AppEnvironment` deliberately does not rewrite the process environment
/// that Git inherits. Run each application regression in a fresh process with
/// Git's system and global files disabled, so a developer's configuration
/// cannot become fixture input or race another test changing its environment.
fn run_in_isolated_test_process(name: &str) -> bool {
    if std::env::var_os("SKILLED_WINDOWS_GUARD_CHILD").is_some() {
        return false;
    }
    let isolated = tempfile::tempdir().expect("test process environment");
    let home = isolated.path().join("home");
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", name, "--nocapture"])
        .env("SKILLED_WINDOWS_GUARD_CHILD", "1")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("GIT_CONFIG_GLOBAL", home.join("global-gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run isolated regression process");
    assert!(
        output.status.success(),
        "isolated test {name} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

/// Git for Windows applies `core.unsetenvvars` only when it starts a child.
/// The harmless SSH recorder proves each guard reaches that child normally and
/// that a configuration entry removes exactly the named guard before spawn.
#[test]
fn git_for_windows_removes_each_guard_before_the_ssh_child() {
    if run_in_isolated_test_process("git_for_windows_removes_each_guard_before_the_ssh_child") {
        return;
    }
    let fixture = fixture();
    let baseline = recorded_environment(&fixture, None);
    for (guard, _) in GUARDS {
        assert!(
            baseline.get(*guard).is_some_and(|value| !value.is_empty()),
            "{guard}: {baseline:?}"
        );
        let stripped = recorded_environment(&fixture, Some(guard));
        assert_eq!(stripped.get(*guard), Some(&String::new()), "{guard}");
    }
}

/// Skilled rejects each checkout-controlled removal before the update reaches
/// the fetch that would start a Git transport child.
#[test]
fn a_checkout_cannot_strip_any_git_child_process_guard_on_windows() {
    if run_in_isolated_test_process(
        "a_checkout_cannot_strip_any_git_child_process_guard_on_windows",
    ) {
        return;
    }
    for guard in PROTECTED_GUARDS {
        let fixture = fixture();
        fixture.git(&fixture.clone, &["config", "core.unsetenvvars", guard]);
        let probe = probe_repository_update(&fixture.app.sources()[0], true);
        let (verdict, findings) = classify_repository_update(&probe);
        assert_eq!(
            verdict,
            RepositoryUpdateVerdict::Blocked,
            "{guard}: {findings:?}"
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.code() == "source.repository_transport_unsupported"),
            "{guard}: {findings:?}"
        );
    }
}
