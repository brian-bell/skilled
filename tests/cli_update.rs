#![cfg(unix)]

use std::{
    io::{BufRead, Cursor, Read},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

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

struct StaleConfirmation {
    input: Cursor<Vec<u8>>,
    checkout: PathBuf,
    database_to_break: Option<PathBuf>,
    cache_to_break: Option<PathBuf>,
    changed: bool,
}

impl StaleConfirmation {
    fn change_checkout(&mut self) {
        if !self.changed {
            std::fs::write(
                self.checkout.join("skills/demo/SKILL.md"),
                "changed after preview\n",
            )
            .expect("change checkout after preview");
            if let Some(database) = &self.database_to_break {
                let connection = rusqlite::Connection::open(database).expect("open metadata");
                connection
                    .execute_batch("DROP TABLE source_repositories;")
                    .expect("make source refresh unavailable");
            }
            if let Some(database) = &self.cache_to_break {
                let connection = rusqlite::Connection::open(database).expect("open metadata");
                connection
                    .execute_batch(
                        "CREATE TRIGGER reject_update_check BEFORE UPDATE ON source_update_checks
                         BEGIN SELECT RAISE(FAIL, 'fixture cache failure'); END;",
                    )
                    .expect("make update cache unavailable");
            }
            self.changed = true;
        }
    }
}

impl Read for StaleConfirmation {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.change_checkout();
        self.input.read(buffer)
    }
}

impl BufRead for StaleConfirmation {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        self.change_checkout();
        self.input.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.input.consume(amount);
    }
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

#[test]
fn a_guard_refusal_after_confirmation_is_blocked_without_claiming_a_failed_write() {
    let (_temporary, environment, seed, clone) = fixture();
    std::fs::write(seed.join("upstream.txt"), "incoming\n").expect("incoming file");
    commit(&seed, "upstream change");
    git(&seed, &["push"]);
    let before = git(&clone, &["rev-parse", "HEAD"]);
    let mut input = StaleConfirmation {
        input: Cursor::new(b"y\n".to_vec()),
        checkout: clone.clone(),
        database_to_break: None,
        cache_to_break: None,
        changed: false,
    };
    let mut output = Vec::new();

    let code = cli::run(
        &[
            "update".into(),
            "--source".into(),
            clone.display().to_string(),
        ],
        environment,
        &mut input,
        &mut output,
    );

    let output = String::from_utf8(output).expect("utf-8 output");
    assert_eq!(code, ExitCodeKind::Blocked, "{output}");
    assert_eq!(git(&clone, &["rev-parse", "HEAD"]), before);
    assert!(output.contains("Blocked: nothing was written."), "{output}");
    assert!(output.contains("Guard refusal: source changed"), "{output}");
    assert!(!output.contains("Fast-forward command failed"), "{output}");
}

#[test]
fn a_guard_refusal_with_an_unavailable_refresh_is_not_reported_as_an_ordinary_block() {
    let (temporary, environment, seed, clone) = fixture();
    std::fs::write(seed.join("upstream.txt"), "incoming\n").expect("incoming file");
    commit(&seed, "upstream change");
    git(&seed, &["push"]);
    let mut input = StaleConfirmation {
        input: Cursor::new(b"y\n".to_vec()),
        checkout: clone,
        database_to_break: Some(temporary.path().join("data/skilled.sqlite3")),
        cache_to_break: None,
        changed: false,
    };
    let mut output = Vec::new();

    let code = cli::run(
        &["update".into(), "--source".into(), "1".into()],
        environment,
        &mut input,
        &mut output,
    );

    let output = String::from_utf8(output).expect("utf-8 output");
    assert_eq!(code, ExitCodeKind::PartialApply, "{output}");
    assert!(output.contains("Guard refusal: source changed"), "{output}");
    assert!(
        output.contains("Post-attempt state unavailable"),
        "{output}"
    );
    assert!(output.contains("nothing was written"), "{output}");
}

#[test]
fn a_guard_refusal_with_a_cache_failure_keeps_the_refreshed_state_distinct() {
    let (temporary, environment, seed, clone) = fixture();
    std::fs::write(seed.join("upstream.txt"), "incoming\n").expect("incoming file");
    commit(&seed, "upstream change");
    git(&seed, &["push"]);
    let mut input = StaleConfirmation {
        input: Cursor::new(b"y\n".to_vec()),
        checkout: clone,
        database_to_break: None,
        cache_to_break: Some(temporary.path().join("data/skilled.sqlite3")),
        changed: false,
    };
    let mut output = Vec::new();

    let code = cli::run(
        &["update".into(), "--source".into(), "1".into()],
        environment,
        &mut input,
        &mut output,
    );

    let output = String::from_utf8(output).expect("utf-8 output");
    assert_eq!(code, ExitCodeKind::PartialApply, "{output}");
    assert!(
        output.contains("Post-attempt state was not cached"),
        "{output}"
    );
    assert!(!output.contains("state unavailable"), "{output}");
}

/// Verification has three answers, and the exit status has to keep them apart.
/// A report that nothing disagreed with is not a report that every
/// postcondition was established: the withheld details are printed, but a
/// script reads the status, and `0` there would present an update whose core
/// postconditions were never checked as an ordinary success.
#[test]
fn an_incomplete_verification_does_not_exit_as_a_plain_success() {
    let (temporary, environment, seed, clone) = fixture();
    if running_as_root() {
        // Permission bits do not bind the superuser, so the unreadable root
        // this test needs cannot be created.
        return;
    }
    std::fs::write(seed.join("skills/demo/new.txt"), "new\n").expect("incoming file");
    commit(&seed, "upstream change");
    git(&seed, &["push"]);
    let target = git(&seed, &["rev-parse", "HEAD"]);
    // A skill root the scan cannot read leaves the before-and-after inventory
    // comparison withheld, which is exactly the incomplete-but-not-failed case.
    let root = temporary.path().join("home/.claude/skills");
    std::fs::create_dir_all(&root).expect("agent skill root");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o000))
        .expect("seal the agent skill root");

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
    // Restore before any assertion can leave the fixture undeletable.
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755))
        .expect("restore permissions");

    let output = String::from_utf8(output).expect("utf-8 output");
    assert_eq!(code, ExitCodeKind::VerificationIncomplete, "{output}");
    assert_ne!(code.code(), 0);
    assert_eq!(git(&clone, &["rev-parse", "HEAD"]), target);
    assert!(
        output.contains("Verified as far as it could be."),
        "{output}"
    );
    assert!(output.contains("Not established:"), "{output}");
}

/// A row stored before schema 9 has no recorded repository identity, and a
/// different clone of the same repository standing at the registered path
/// contains the stored head just as the original would. Nothing can tell the
/// two apart, so the identity must never be adopted from whatever is standing
/// there, and an update against the row is refused with re-registration as
/// the remedy (skilled-t0f).
#[test]
fn a_pre_identity_source_is_refused_an_update_and_never_adopts_the_standing_checkout() {
    let (temporary, environment, _seed, clone) = fixture();
    let database = temporary.path().join("data/skilled.sqlite3");
    let stored_identity = || -> Option<String> {
        let connection = rusqlite::Connection::open(&database).expect("open metadata");
        connection
            .query_row(
                "SELECT repository_identity FROM source_repositories",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .expect("read stored identity")
    };
    assert!(stored_identity().is_some(), "registration records identity");
    {
        let connection = rusqlite::Connection::open(&database).expect("open metadata");
        connection
            .execute(
                "UPDATE source_repositories SET repository_identity = NULL",
                [],
            )
            .expect("simulate a pre-schema-9 row");
    }
    // A different clone of the same repository, holding the same head, takes
    // the registered path.
    let remote = temporary.path().join("remote.git");
    let replacement = temporary.path().join("replacement");
    let output = Command::new("git")
        .args(["clone", "--branch", "main"])
        .arg(&remote)
        .arg(&replacement)
        .output()
        .expect("clone replacement");
    assert!(output.status.success(), "{output:?}");
    std::fs::remove_dir_all(&clone).expect("remove registered checkout");
    std::fs::rename(&replacement, &clone).expect("stand the replacement at the path");

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
    let output = String::from_utf8(output).expect("utf-8 output");

    assert_eq!(code, ExitCodeKind::Blocked, "{output}");
    assert!(output.contains("source.identity_unproven"), "{output}");
    assert!(output.contains("re-register"), "{output}");
    // The standing checkout's identity was not recorded: the row still says
    // it was never proven.
    assert_eq!(stored_identity(), None);

    // The recorded check is what the Updates list advertises and what Doctor
    // reads, and a fresh check would only repeat this refusal: nothing the
    // source reports about the standing checkout may supersede it.
    let reopened = SkilledApp::open(environment.clone()).expect("reopen app");
    assert_eq!(reopened.update_checks().len(), 1);
    let check = &reopened.update_checks()[0];
    assert!(
        !check.superseded_by(&reopened.sources()[0]),
        "the identity-unproven check must outlive the source re-read"
    );
    assert!(
        check
            .findings()
            .iter()
            .any(|finding| finding.code() == "source.identity_unproven"),
        "{:?}",
        check.findings()
    );
    drop(reopened);

    // The condition the check records is the absence of a recorded identity,
    // and no observation of the standing checkout changes that: a moved HEAD
    // would only make a fresh check repeat the same refusal, so it does not
    // supersede this one.
    std::fs::write(clone.join("local.txt"), "local\n").expect("local change");
    commit(&clone, "local commit");
    let moved = SkilledApp::open(environment).expect("reopen after HEAD moved");
    assert!(
        !moved.update_checks()[0].superseded_by(&moved.sources()[0]),
        "a moved HEAD must not supersede an identity-unproven check"
    );
}

/// The other half of the same bead: a pre-schema-9 row whose path still holds
/// the originally registered checkout keeps every non-update surface working
/// without user action, and re-registering the checkout — the stated remedy —
/// records its identity and restores updates.
#[test]
fn re_registering_a_pre_identity_source_restores_updates() {
    let (temporary, environment, seed, clone) = fixture();
    let database = temporary.path().join("data/skilled.sqlite3");
    {
        let connection = rusqlite::Connection::open(&database).expect("open metadata");
        connection
            .execute(
                "UPDATE source_repositories SET repository_identity = NULL",
                [],
            )
            .expect("simulate a pre-schema-9 row");
    }
    // The registry still loads the source without an error: the checkout is
    // the one registered, and only updates are gated on the proof.
    let app = SkilledApp::open(environment.clone()).expect("open app");
    assert_eq!(app.sources().len(), 1);
    assert!(app.sources()[0].source_error().is_none());
    drop(app);

    // An explicit check first, so the refusal is on record the way Updates
    // and Doctor would hold it.
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
        ExitCodeKind::Blocked,
        "{}",
        String::from_utf8_lossy(&output)
    );

    // Re-registering the same path records the identity again — and that is
    // exactly the condition the cached refusal tracks, so recording it is
    // what supersedes the check.
    let mut app = SkilledApp::open(environment.clone()).expect("reopen app");
    let preview = app.preview_source(&clone).expect("preview same checkout");
    app.confirm_source(preview).expect("re-register checkout");
    drop(app);
    let after = SkilledApp::open(environment.clone()).expect("reopen after re-registration");
    assert!(
        after.update_checks()[0].superseded_by(&after.sources()[0]),
        "re-registration must supersede the identity-unproven check"
    );
    drop(after);
    let connection = rusqlite::Connection::open(&database).expect("open metadata");
    let recorded: Option<String> = connection
        .query_row(
            "SELECT repository_identity FROM source_repositories",
            [],
            |row| row.get(0),
        )
        .expect("read stored identity");
    drop(connection);
    assert!(recorded.is_some(), "re-registration records identity");

    std::fs::write(seed.join("skills/demo/new.txt"), "new\n").expect("incoming file");
    commit(&seed, "upstream change");
    git(&seed, &["push"]);
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
    let output = String::from_utf8(output).expect("utf-8 output");
    assert_eq!(code, ExitCodeKind::Success, "{output}");
    assert!(clone.join("skills/demo/new.txt").is_file());
}

fn running_as_root() -> bool {
    // Reading a mode-0 directory succeeds only for the superuser.
    let probe = std::env::temp_dir().join(format!("skilled-root-probe-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&probe);
    std::fs::create_dir(&probe).expect("create permission probe");
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o000)).expect("seal probe");
    let readable = std::fs::read_dir(&probe).is_ok();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755)).expect("unseal probe");
    std::fs::remove_dir_all(&probe).expect("remove probe");
    readable
}

/// The cached check is what the Updates list advertises and what Doctor reads,
/// so a finding the preview would block on has to be recorded by the check
/// itself. Otherwise the list keeps offering an update the preview then
/// refuses, and the finding never reaches Doctor at all.
#[test]
fn a_removal_blocker_reaches_the_cached_check_and_not_only_the_preview() {
    let (temporary, environment, seed, clone) = fixture();
    let root = temporary.path().join("home/.claude/skills");
    std::fs::create_dir_all(&root).expect("agent root");
    std::os::unix::fs::symlink(clone.join("skills/demo"), root.join("demo"))
        .expect("installed skill");

    std::fs::remove_dir_all(seed.join("skills/demo")).expect("remove skill upstream");
    commit(&seed, "remove demo");
    git(&seed, &["push"]);
    std::fs::write(clone.join("skills/demo/notes.txt"), "mine\n").expect("local occupant");
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
    let output = String::from_utf8(output).expect("utf-8 output");
    assert_eq!(code, ExitCodeKind::Blocked, "{output}");
    assert!(output.contains("source.removal_leaves_content"), "{output}");
    assert_eq!(git(&clone, &["rev-parse", "HEAD"]), before);

    let reopened = SkilledApp::open(environment).expect("reopen app");
    let check = reopened
        .update_checks()
        .first()
        .expect("the check the command recorded");
    assert_eq!(check.verdict, RepositoryUpdateVerdict::Blocked);
    assert!(
        check
            .findings()
            .iter()
            .any(|finding| finding.code() == "source.removal_leaves_content"),
        "{:?}",
        check.findings()
    );
}

/// Naming the alias is not enough. What a rename does to a link installed under
/// a name of its own is leave it with nothing to resolve to, and that is the
/// outcome verification holds the update to — so it is what the confirmation
/// has to state, not merely which link is involved.
#[test]
fn a_rename_states_that_it_leaves_an_aliased_installation_without_a_target() {
    let (temporary, environment, seed, clone) = fixture();
    let root = temporary.path().join("home/.claude/skills");
    std::fs::create_dir_all(&root).expect("agent root");
    std::os::unix::fs::symlink(clone.join("skills/demo"), root.join("alias"))
        .expect("installed under another name");

    std::fs::rename(seed.join("skills/demo"), seed.join("skills/renamed"))
        .expect("rename skill upstream");
    commit(&seed, "rename demo");
    git(&seed, &["push"]);

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
    let output = String::from_utf8(output).expect("utf-8 output");
    assert_eq!(code, ExitCodeKind::Success, "{output}");
    assert!(output.contains("renamed · demo -> renamed"), "{output}");
    assert!(output.contains("loses its target · alias"), "{output}");
}
