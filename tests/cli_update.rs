#![cfg(unix)]

use std::{io::Cursor, path::Path, process::Command};

use skilled::{
    AppEnvironment, SkilledApp,
    cli::{self, ExitCodeKind},
    updates::RepositoryUpdateVerdict,
};

fn git(repository: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .expect("invoke git");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn commit(repository: &Path, message: &str) {
    git(repository, &["add", "."]);
    git(
        repository,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-m",
            message,
        ],
    );
}

fn fixture() -> (
    tempfile::TempDir,
    AppEnvironment,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let remote = temporary.path().join("remote.git");
    let seed = temporary.path().join("seed");
    let clone = temporary.path().join("clone");
    Command::new("git")
        .args(["init", "--bare"])
        .arg(&remote)
        .output()
        .expect("bare remote");
    Command::new("git")
        .args(["init", "-b", "main"])
        .arg(&seed)
        .output()
        .expect("seed repository");
    std::fs::create_dir_all(seed.join("skills/demo")).expect("skill directory");
    std::fs::write(
        seed.join("skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: fixture\n---\n",
    )
    .expect("skill");
    commit(&seed, "initial");
    git(
        &seed,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&seed, &["push", "-u", "origin", "main"]);
    Command::new("git")
        .args(["clone", "--branch", "main"])
        .arg(&remote)
        .arg(&clone)
        .output()
        .expect("clone");
    let environment = AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    );
    let mut app = SkilledApp::open(environment.clone()).expect("open app");
    let preview = app.preview_source(&clone).expect("preview clone");
    app.confirm_source(preview).expect("register clone");
    (temporary, environment, seed, clone)
}

#[test]
fn a_clean_behind_clone_is_previewed_fast_forwarded_and_verified() {
    let (_temporary, environment, seed, clone) = fixture();
    std::fs::write(seed.join("skills/demo/new.txt"), "new\n").expect("incoming file");
    commit(&seed, "upstream change");
    git(&seed, &["push"]);
    let target = git(&seed, &["rev-parse", "HEAD"]);
    let before = git(&clone, &["rev-parse", "HEAD"]);
    let mut input = Cursor::new(Vec::<u8>::new());
    let mut output = Vec::new();
    let code = cli::run(
        &[
            "update".into(),
            "--source".into(),
            clone.display().to_string(),
            "--yes".into(),
        ],
        environment.clone(),
        &mut input,
        &mut output,
    );
    assert_eq!(
        code,
        ExitCodeKind::Success,
        "{}",
        String::from_utf8_lossy(&output)
    );
    assert_ne!(before, target);
    assert_eq!(git(&clone, &["rev-parse", "HEAD"]), target);
    assert!(clone.join("skills/demo/new.txt").is_file());
    let reflog = git(&clone, &["reflog", "-1", "--format=%gs"]);
    assert!(reflog.contains("Fast-forward"), "{reflog}");
    let output = String::from_utf8(output).expect("utf-8 output");
    // A typed command reads the agent roots whether or not setup has run, both
    // before the plan and after the write, exactly as `skilled install` does.
    // Two snapshots of equal standing are what let it state a verified result
    // instead of withholding one, so nothing here is partial.
    assert!(
        output.contains("affected installations: complete"),
        "{output}"
    );
    assert!(output.contains("branch refs/heads/main"), "{output}");
    assert!(
        output.contains("Verified: HEAD is the previewed revision."),
        "{output}"
    );

    let reopened = SkilledApp::open(environment).expect("reopen app");
    assert_eq!(reopened.update_checks().len(), 1);
    assert_eq!(
        reopened.update_checks()[0].verdict,
        RepositoryUpdateVerdict::UpToDate
    );
    assert!(
        reopened.update_checks()[0].detail.is_empty(),
        "{}",
        reopened.update_checks()[0].detail
    );
}

#[test]
fn tracked_worktree_changes_block_without_moving_head() {
    let (_temporary, environment, seed, clone) = fixture();
    std::fs::write(seed.join("upstream.txt"), "incoming\n").expect("incoming file");
    commit(&seed, "upstream change");
    git(&seed, &["push"]);
    std::fs::write(clone.join("skills/demo/SKILL.md"), "locally changed\n").expect("dirty clone");
    let before = git(&clone, &["rev-parse", "HEAD"]);
    let mut input = Cursor::new(Vec::<u8>::new());
    let mut output = Vec::new();
    let code = cli::run(
        &[
            "update".into(),
            "--source".into(),
            clone.display().to_string(),
            "--yes".into(),
        ],
        environment,
        &mut input,
        &mut output,
    );
    assert_eq!(
        code,
        ExitCodeKind::Blocked,
        "{}",
        String::from_utf8_lossy(&output)
    );
    assert_eq!(git(&clone, &["rev-parse", "HEAD"]), before);
    assert!(String::from_utf8_lossy(&output).contains("source.dirty"));
}
