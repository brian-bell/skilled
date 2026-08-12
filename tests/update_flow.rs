#![cfg(unix)]

use std::time::{Duration, Instant};
use std::{path::Path, process::Command};

use ratatui::{Terminal, backend::TestBackend};
use skilled::{
    Action, AppEnvironment, Effect, SkilledApp, tui,
    updates::{
        RepositoryUpdatePrompt, RepositoryUpdateVerdict, apply_repository_update,
        classify_repository_update, plan_repository_update, probe_repository_update,
        verify_repository_update,
    },
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

struct Fixture {
    _temporary: tempfile::TempDir,
    app: SkilledApp,
    remote: std::path::PathBuf,
    seed: std::path::PathBuf,
    clone: std::path::PathBuf,
}

fn fixture() -> Fixture {
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
    std::fs::write(seed.join("skills/demo/old.txt"), "old\n").expect("old skill file");
    std::fs::create_dir_all(seed.join("skills/other")).expect("other skill directory");
    std::fs::write(
        seed.join("skills/other/SKILL.md"),
        "---\nname: other\ndescription: fixture\n---\n",
    )
    .expect("other skill");
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
    let mut app = SkilledApp::open(environment).expect("open app");
    let preview = app.preview_source(&clone).expect("preview clone");
    app.confirm_source(preview).expect("register clone");
    Fixture {
        _temporary: temporary,
        app,
        remote,
        seed,
        clone,
    }
}

fn push_update(fixture: &Fixture, path: &str) -> String {
    std::fs::write(fixture.seed.join(path), "incoming\n").expect("incoming file");
    commit(&fixture.seed, "upstream change");
    git(&fixture.seed, &["push"]);
    git(&fixture.seed, &["rev-parse", "HEAD"])
}

#[test]
fn a_locally_ahead_repository_has_no_executable_update_plan() {
    let fixture = fixture();
    std::fs::write(fixture.clone.join("local.txt"), "local\n").expect("local file");
    commit(&fixture.clone, "local change");
    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);

    let (verdict, findings) = classify_repository_update(&probe);
    let plan =
        plan_repository_update(&source, &probe, fixture.app.inventory()).expect("plan ahead");

    assert_eq!(verdict, RepositoryUpdateVerdict::Ahead);
    assert!(findings.is_empty());
    assert_eq!(plan.current_revision(), plan.target_revision());
    assert!(plan.commits().is_empty());
    assert!(plan.changed_files().is_empty());
}

#[test]
fn a_partial_clone_is_blocked_before_update_inspection_or_fetch() {
    let fixture = fixture();
    git(
        &fixture.clone,
        &["config", "core.repositoryformatversion", "1"],
    );
    git(
        &fixture.clone,
        &["config", "extensions.partialClone", "origin"],
    );

    let probe = probe_repository_update(&fixture.app.sources()[0], true);
    let (verdict, findings) = classify_repository_update(&probe);

    assert_eq!(verdict, RepositoryUpdateVerdict::Blocked);
    assert!(
        findings
            .iter()
            .any(|finding| finding.code() == "source.partial_clone_unsupported")
    );
}

/// A second promisor remote is configured without the extension key, and Git
/// lazily fetches from it just the same, so the marker alone has to block the
/// update rather than only the spelling a filtered clone leaves behind.
#[test]
fn a_promisor_remote_without_the_extension_key_is_blocked_the_same_way() {
    let fixture = fixture();
    git(
        &fixture.clone,
        &["config", "remote.origin.promisor", "true"],
    );

    let probe = probe_repository_update(&fixture.app.sources()[0], true);
    let (verdict, findings) = classify_repository_update(&probe);

    assert_eq!(verdict, RepositoryUpdateVerdict::Blocked);
    assert!(
        findings
            .iter()
            .any(|finding| finding.code() == "source.partial_clone_unsupported"),
        "{findings:?}"
    );
}

/// A filter recorded by a filtered fetch is the same warning as the extension.
#[test]
fn a_recorded_partial_clone_filter_blocks_the_update() {
    let fixture = fixture();
    git(
        &fixture.clone,
        &["config", "remote.origin.partialCloneFilter", "blob:none"],
    );

    let probe = probe_repository_update(&fixture.app.sources()[0], true);
    let (verdict, _) = classify_repository_update(&probe);

    assert_eq!(verdict, RepositoryUpdateVerdict::Blocked);
}

/// A marker Git itself reads as off is not a promisor remote, and refusing it
/// would block an ordinary repository from an update it can take.
#[test]
fn a_promisor_marker_turned_off_does_not_block_the_update() {
    let fixture = fixture();
    git(
        &fixture.clone,
        &["config", "remote.origin.promisor", "false"],
    );

    let probe = probe_repository_update(&fixture.app.sources()[0], true);
    let (verdict, findings) = classify_repository_update(&probe);

    assert_ne!(verdict, RepositoryUpdateVerdict::Blocked);
    assert!(
        findings
            .iter()
            .all(|finding| finding.code() != "source.partial_clone_unsupported"),
        "{findings:?}"
    );
}

/// A check that refused before reading HEAD holds no reference of its own, and
/// judging that absence against the branch the source reports would supersede
/// the refusal the instant it was recorded — leaving Doctor silent about a
/// repository every later check would refuse again for the same reason.
#[test]
fn a_partial_clone_refusal_is_current_and_visible_in_doctor() {
    let mut fixture = fixture();
    git(
        &fixture.clone,
        &["config", "remote.origin.promisor", "true"],
    );

    fixture
        .app
        .perform_effects(&[Effect::CheckUpdates])
        .expect("start check");
    finish_update_check(&mut fixture.app);

    let check = &fixture.app.update_checks()[0];
    assert_eq!(check.verdict, RepositoryUpdateVerdict::Blocked);
    assert!(!check.superseded_by(&fixture.app.sources()[0]));
    assert!(
        fixture
            .app
            .doctor_findings()
            .iter()
            .any(|entry| entry.finding().code() == "source.partial_clone_unsupported")
    );
}

/// Git runs the repository's `reference-transaction` hook whenever a fetch
/// updates the remote-tracking ref. A check is offered as reading a repository,
/// and the hook disclosure the preview carries speaks of the fast-forward, so
/// the check itself must not run repository code.
#[test]
fn an_update_check_runs_no_reference_transaction_hook() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = fixture();
    let sentinel = fixture._temporary.path().join("hook-ran");
    let hook = fixture.clone.join(".git/hooks/reference-transaction");
    std::fs::write(
        &hook,
        format!("#!/bin/sh\ntouch '{}'\n", sentinel.display()),
    )
    .expect("hook fixture");
    let mut permissions = std::fs::metadata(&hook)
        .expect("hook metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&hook, permissions).expect("executable hook");
    // A checkout is not always one this machine cloned, so where the hook
    // search is sent has to be somewhere the checkout cannot reach: a hook
    // waiting inside its Git directory may not be run either.
    let planted = fixture
        .clone
        .join(".git/skilled-suppressed-hooks/reference-transaction");
    std::fs::create_dir_all(planted.parent().expect("planted hook directory"))
        .expect("planted hook directory");
    std::fs::write(
        &planted,
        format!("#!/bin/sh\ntouch '{}'\n", sentinel.display()),
    )
    .expect("planted hook");
    let mut permissions = std::fs::metadata(&planted)
        .expect("planted hook metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&planted, permissions).expect("executable planted hook");
    let target = push_update(&fixture, "skills/demo/new.txt");

    let probe = probe_repository_update(&fixture.app.sources()[0], true);
    let (verdict, _) = classify_repository_update(&probe);

    assert_eq!(verdict, RepositoryUpdateVerdict::Available);
    assert!(!sentinel.exists(), "the check ran a repository hook");
    // The tracking ref still has to advance: suppressing hooks may not turn the
    // fetch into one that observes nothing.
    assert_eq!(
        git(&fixture.clone, &["rev-parse", "refs/remotes/origin/main"]),
        target
    );
}

/// Every changed path matching an exact rename does not make the move whole:
/// what did not change stayed where it was. Calling that a rename would have
/// verification expect the old skill's link to dangle while the skill is still
/// there, failing an update that did exactly what it said.
#[test]
fn moving_one_file_out_of_a_skill_that_remains_is_not_a_rename() {
    let mut fixture = fixture();
    let root = fixture._temporary.path().join("home/.claude/skills");
    std::fs::create_dir_all(&root).expect("agent root");
    std::os::unix::fs::symlink(fixture.clone.join("skills/other"), root.join("other"))
        .expect("installed skill");
    fixture
        .app
        .perform_effects(&[Effect::ScanInstallations])
        .expect("scan before update");

    // The file has to be in the clone before it can be seen to move out of it.
    std::fs::write(
        fixture.seed.join("skills/other/extra.md"),
        "content that moves whole\n",
    )
    .expect("extra file");
    commit(&fixture.seed, "add a file inside an existing skill");
    git(&fixture.seed, &["push"]);
    git(&fixture.clone, &["pull", "--ff-only"]);

    std::fs::create_dir_all(fixture.seed.join("skills/moved")).expect("destination skill");
    std::fs::rename(
        fixture.seed.join("skills/other/extra.md"),
        fixture.seed.join("skills/moved/extra.md"),
    )
    .expect("move one file");
    commit(&fixture.seed, "move one file out of a skill");
    git(&fixture.seed, &["push"]);

    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan =
        plan_repository_update(&source, &probe, fixture.app.inventory()).expect("plan update");

    assert!(plan.affected().renamed.is_empty(), "{:?}", plan.affected());
    assert!(plan.affected().removed.is_empty(), "{:?}", plan.affected());
}

/// A file added inside a skill the source already carries does not make that
/// skill new, and a preview that said otherwise would be asking for consent to
/// something that is not happening.
#[test]
fn a_file_added_to_an_existing_uninstalled_skill_is_not_reported_as_added() {
    let fixture = fixture();
    std::fs::write(fixture.seed.join("skills/other/extra.md"), "extra\n").expect("extra file");
    commit(&fixture.seed, "add a file inside an existing skill");
    git(&fixture.seed, &["push"]);

    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan =
        plan_repository_update(&source, &probe, fixture.app.inventory()).expect("plan update");

    assert!(plan.affected().added.is_empty(), "{:?}", plan.affected());
}

/// The configured remote-tracking ref is the whole destination of a check, and
/// FETCH_HEAD is state the user's own fetches leave behind.
#[test]
fn an_update_check_leaves_fetch_head_alone() {
    let fixture = fixture();
    let fetch_head = fixture.clone.join(".git/FETCH_HEAD");
    std::fs::write(&fetch_head, "user fetch state\n").expect("fetch head fixture");
    push_update(&fixture, "skills/demo/new.txt");

    let probe = probe_repository_update(&fixture.app.sources()[0], true);
    let (verdict, _) = classify_repository_update(&probe);

    assert_eq!(verdict, RepositoryUpdateVerdict::Available);
    assert_eq!(
        std::fs::read_to_string(&fetch_head).expect("fetch head after check"),
        "user fetch state\n"
    );
}

/// Git resolves a symbolic ref before updating it, so fetching into a
/// remote-tracking ref that is one would move whatever it points at. A check
/// is only ever allowed to advance the tracking ref itself.
#[test]
fn a_symbolic_remote_tracking_ref_is_refused_before_any_fetch() {
    let fixture = fixture();
    git(&fixture.clone, &["branch", "work"]);
    let before = git(&fixture.clone, &["rev-parse", "refs/heads/work"]);
    git(
        &fixture.clone,
        &[
            "symbolic-ref",
            "refs/remotes/origin/main",
            "refs/heads/work",
        ],
    );
    push_update(&fixture, "skills/demo/new.txt");

    let probe = probe_repository_update(&fixture.app.sources()[0], true);
    let (verdict, findings) = classify_repository_update(&probe);

    assert_eq!(verdict, RepositoryUpdateVerdict::Blocked);
    assert!(
        findings
            .iter()
            .any(|finding| finding.code() == "source.fetch_failed"
                && finding.evidence().contains("symbolic ref")),
        "{findings:?}"
    );
    assert_eq!(
        git(&fixture.clone, &["rev-parse", "refs/heads/work"]),
        before
    );
}

/// `git symbolic-ref --short` prints the shortest spelling that resolves back
/// to HEAD, so a tag named after the branch turns `main` into `heads/main`. A
/// check of that repository still describes the branch it was taken on.
#[test]
fn a_branch_sharing_a_name_with_a_tag_does_not_supersede_its_own_check() {
    let fixture = fixture();
    push_update(&fixture, "skills/demo/new.txt");
    git(&fixture.clone, &["tag", "main", "HEAD"]);
    // Re-opened so the registered source is inspected with the tag in place:
    // that is what makes Git spell the branch `heads/main`.
    let mut app = SkilledApp::open(AppEnvironment::new(
        fixture._temporary.path().join("home"),
        fixture._temporary.path().join("data"),
        "",
    ))
    .expect("reopen app");

    app.perform_effects(&[Effect::CheckUpdates])
        .expect("start check");
    finish_update_check(&mut app);

    let source = app.sources()[0].clone();
    assert_eq!(source.branch(), Some("heads/main"));
    let check = &app.update_checks()[0];
    assert!(!check.superseded_by(&source));
    assert_eq!(check.verdict, RepositoryUpdateVerdict::Available);
}

#[test]
fn update_status_does_not_invoke_repository_configured_clean_filters() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = fixture();
    let sentinel = fixture._temporary.path().join("filter-ran");
    let filter = fixture._temporary.path().join("clean-filter");
    std::fs::write(
        &filter,
        format!("#!/bin/sh\ntouch '{}'\ncat\n", sentinel.display()),
    )
    .expect("filter fixture");
    let mut permissions = std::fs::metadata(&filter)
        .expect("filter metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&filter, permissions).expect("executable filter");
    std::fs::write(
        fixture.clone.join(".gitattributes"),
        "old.txt filter=sentinel\n",
    )
    .expect("attributes");
    git(
        &fixture.clone,
        &["config", "filter.sentinel.clean", filter.to_str().unwrap()],
    );
    git(
        &fixture.clone,
        &["config", "filter.sentinel.required", "true"],
    );

    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, false);

    assert!(probe.error.is_none(), "{:?}", probe.error);
    assert!(
        !sentinel.exists(),
        "the update status probe invoked a repository-defined clean filter"
    );
}

#[test]
fn a_staged_rename_is_classified_as_dirty_not_as_a_fetch_failure() {
    let fixture = fixture();
    push_update(&fixture, "skills/demo/new.txt");
    git(
        &fixture.clone,
        &["mv", "skills/demo/old.txt", "skills/demo/renamed.txt"],
    );
    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let (verdict, findings) = classify_repository_update(&probe);

    assert_eq!(verdict, RepositoryUpdateVerdict::Blocked);
    assert!(
        findings
            .iter()
            .any(|finding| finding.code() == "source.dirty")
    );
    assert!(
        !findings
            .iter()
            .any(|finding| finding.code() == "source.fetch_failed")
    );
}

#[test]
fn filter_ambiguous_worktree_changes_are_reported_as_unknown_not_dirty() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = fixture();
    let filter = fixture._temporary.path().join("uppercase-filter");
    std::fs::write(&filter, "#!/bin/sh\ntr '[:lower:]' '[:upper:]'\n").expect("filter fixture");
    let mut permissions = std::fs::metadata(&filter)
        .expect("filter metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&filter, permissions).expect("executable filter");
    git(
        &fixture.clone,
        &["config", "filter.upper.clean", filter.to_str().unwrap()],
    );
    git(&fixture.clone, &["config", "filter.upper.required", "true"]);
    std::fs::write(
        fixture.clone.join(".gitattributes"),
        "filtered.txt filter=upper\n",
    )
    .expect("attributes");
    std::fs::write(fixture.clone.join("filtered.txt"), "lowercase\n").expect("filtered file");
    commit(&fixture.clone, "filtered content");
    std::fs::write(fixture.clone.join("filtered.txt"), "lowerCase\n")
        .expect("filter-equivalent worktree content");
    let blob = git(&fixture.clone, &["rev-parse", ":filtered.txt"]);
    git(
        &fixture.clone,
        &[
            "update-index",
            "--cacheinfo",
            &format!("100644,{blob},filtered.txt"),
        ],
    );

    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, false);
    let state = probe.worktree.as_ref().expect("worktree state");
    let (_, findings) = classify_repository_update(&probe);

    assert!(!state.worktree_dirty);
    assert!(!state.worktree_dirty_known);
    assert_eq!(git(&fixture.clone, &["status", "--porcelain"]), "");
    assert!(findings.iter().any(|finding| {
        finding.code() == "source.dirty" && finding.evidence().contains("could not be determined")
    }));
}

fn finish_update_check(app: &mut SkilledApp) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.update_check_in_flight() && Instant::now() < deadline {
        let effects = app.drain_update_check();
        app.perform_effects(&effects).expect("finish worker");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!app.update_check_in_flight());
}

#[test]
fn switching_to_another_branch_after_preview_cannot_move_that_branch() {
    let fixture = fixture();
    let target = push_update(&fixture, "skills/demo/new.txt");
    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan =
        plan_repository_update(&source, &probe, fixture.app.inventory()).expect("plan update");
    let before = git(&fixture.clone, &["rev-parse", "HEAD"]);
    git(&fixture.clone, &["switch", "-c", "other"]);

    assert!(apply_repository_update(&plan).is_err());
    assert_eq!(git(&fixture.clone, &["rev-parse", "HEAD"]), before);
    assert_eq!(git(&fixture.clone, &["rev-parse", "main"]), before);
    assert_ne!(before, target);
}

#[test]
fn moving_the_tracking_ref_after_preview_blocks_apply() {
    let fixture = fixture();
    push_update(&fixture, "skills/demo/new.txt");
    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan =
        plan_repository_update(&source, &probe, fixture.app.inventory()).expect("plan update");
    let before = git(&fixture.clone, &["rev-parse", "HEAD"]);
    let replacement = git(&fixture.seed, &["rev-parse", "HEAD^"]);
    git(
        &fixture.clone,
        &["update-ref", "refs/remotes/origin/main", &replacement],
    );

    assert!(apply_repository_update(&plan).is_err());
    assert_eq!(git(&fixture.clone, &["rev-parse", "HEAD"]), before);
}

#[test]
fn replacing_the_registered_checkout_with_a_symlink_blocks_apply() {
    let fixture = fixture();
    push_update(&fixture, "skills/demo/new.txt");
    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan =
        plan_repository_update(&source, &probe, fixture.app.inventory()).expect("plan update");
    let attacker = fixture._temporary.path().join("attacker");
    Command::new("git")
        .args(["clone", "--branch", "main"])
        .arg(&fixture.remote)
        .arg(&attacker)
        .output()
        .expect("attacker clone");
    git(&attacker, &["reset", "--hard", plan.current_revision()]);
    let displaced = fixture._temporary.path().join("displaced-clone");
    std::fs::rename(&fixture.clone, &displaced).expect("displace registered checkout");
    std::os::unix::fs::symlink(&attacker, &fixture.clone).expect("replace checkout with symlink");

    assert!(apply_repository_update(&plan).is_err());
    assert_eq!(
        git(&attacker, &["rev-parse", "HEAD"]),
        plan.current_revision()
    );
}

#[test]
fn an_existing_untracked_incoming_path_blocks_the_preview() {
    let mut fixture = fixture();
    push_update(&fixture, "incoming.txt");
    std::fs::write(fixture.clone.join("incoming.txt"), "local\n").expect("untracked file");
    fixture
        .app
        .perform_effects(&[Effect::CheckUpdates])
        .expect("start collision check");
    finish_update_check(&mut fixture.app);
    let cached = &fixture.app.update_checks()[0];
    assert_eq!(cached.verdict, RepositoryUpdateVerdict::Blocked);
    assert!(!cached.superseded_by(&fixture.app.sources()[0]));
    assert!(
        cached.findings().iter().any(|finding| {
            finding.code() == "source.dirty" && finding.evidence().contains("incoming.txt")
        }),
        "{:?}",
        cached.findings()
    );
    assert!(fixture.app.doctor_findings().iter().any(|item| {
        item.finding().code() == "source.dirty"
            && item.finding().evidence().contains("incoming.txt")
    }));

    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, false);
    let plan =
        plan_repository_update(&source, &probe, fixture.app.inventory()).expect("plan update");

    assert!(plan.is_blocked());
    assert!(
        plan.findings()
            .iter()
            .any(|finding| finding.code() == "source.dirty")
    );
}

#[test]
fn an_existing_ignored_incoming_path_blocks_the_preview() {
    let fixture = fixture();
    push_update(&fixture, "ignored.txt");
    std::fs::write(fixture.clone.join(".git/info/exclude"), "ignored.txt\n").expect("ignore rule");
    std::fs::write(fixture.clone.join("ignored.txt"), "local\n").expect("ignored local file");
    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan =
        plan_repository_update(&source, &probe, fixture.app.inventory()).expect("plan update");

    assert!(plan.is_blocked());
    assert!(
        plan.findings()
            .iter()
            .any(|finding| finding.code() == "source.dirty"
                && finding.evidence().contains("ignored.txt"))
    );
    assert_eq!(
        std::fs::read_to_string(fixture.clone.join("ignored.txt")).expect("local content"),
        "local\n"
    );
}

#[test]
fn an_untracked_descendant_created_after_preview_blocks_apply() {
    let fixture = fixture();
    push_update(&fixture, "occupied");
    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan =
        plan_repository_update(&source, &probe, fixture.app.inventory()).expect("plan update");
    assert!(!plan.is_blocked());

    std::fs::create_dir(fixture.clone.join("occupied")).expect("colliding directory");
    std::fs::write(fixture.clone.join("occupied/local.txt"), "local\n")
        .expect("untracked descendant");

    assert!(apply_repository_update(&plan).is_err());
    assert_eq!(
        git(&fixture.clone, &["rev-parse", "HEAD"]),
        plan.current_revision()
    );
}

#[test]
fn an_ignored_path_created_after_preview_blocks_apply() {
    let fixture = fixture();
    push_update(&fixture, "ignored.txt");
    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan =
        plan_repository_update(&source, &probe, fixture.app.inventory()).expect("plan update");
    assert!(!plan.is_blocked());

    std::fs::write(fixture.clone.join(".git/info/exclude"), "ignored.txt\n").expect("ignore rule");
    std::fs::write(fixture.clone.join("ignored.txt"), "local\n").expect("ignored local file");

    assert!(apply_repository_update(&plan).is_err());
    assert_eq!(
        std::fs::read_to_string(fixture.clone.join("ignored.txt")).expect("local content"),
        "local\n"
    );
    assert_eq!(
        git(&fixture.clone, &["rev-parse", "HEAD"]),
        plan.current_revision()
    );
}

#[test]
fn blocked_tui_preview_states_the_finding() {
    let mut fixture = fixture();
    push_update(&fixture, "incoming.txt");
    fixture
        .app
        .perform_effects(&[Effect::CheckUpdates])
        .expect("start check");
    finish_update_check(&mut fixture.app);
    std::fs::write(fixture.clone.join("incoming.txt"), "local\n").expect("late collision");
    fixture
        .app
        .perform_effects(&[Effect::PlanRepositoryUpdate])
        .expect("plan update");
    assert!(matches!(
        fixture.app.pending_update(),
        Some(RepositoryUpdatePrompt::Preview(plan)) if plan.is_blocked()
    ));

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| {
            tui::render(frame, &fixture.app);
        })
        .expect("render preview");
    let buffer = terminal.backend().buffer();
    let rendered = (buffer.area.y..buffer.area.y + buffer.area.height)
        .map(|y| {
            (buffer.area.x..buffer.area.x + buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Blocked: source.dirty"), "{rendered}");
}

#[test]
fn only_observed_installations_are_named() {
    let mut fixture = fixture();
    push_update(&fixture, "skills/demo/new.txt");
    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let before_scan =
        plan_repository_update(&source, &probe, fixture.app.inventory()).expect("plan before scan");
    assert!(before_scan.affected().updated.is_empty());
    assert!(before_scan.affected().complete);

    let root = fixture._temporary.path().join("home/.claude/skills");
    std::fs::create_dir_all(&root).expect("agent root");
    std::os::unix::fs::symlink(fixture.clone.join("skills/demo"), root.join("demo"))
        .expect("installed skill");
    fixture
        .app
        .perform_effects(&[Effect::ScanInstallations])
        .expect("scan installations");

    let after_scan =
        plan_repository_update(&source, &probe, fixture.app.inventory()).expect("plan after scan");
    assert_eq!(after_scan.affected().updated, ["demo"]);
    assert!(after_scan.affected().complete);
}

#[test]
fn deleting_one_file_inside_an_installed_skill_is_an_update_not_a_removal() {
    let mut fixture = fixture();
    let root = fixture._temporary.path().join("home/.claude/skills");
    std::fs::create_dir_all(&root).expect("agent root");
    std::os::unix::fs::symlink(fixture.clone.join("skills/demo"), root.join("demo"))
        .expect("installed skill");
    fixture
        .app
        .perform_effects(&[Effect::ScanInstallations])
        .expect("scan before update");
    let before = fixture.app.inventory().clone();

    std::fs::remove_file(fixture.seed.join("skills/demo/old.txt")).expect("delete old file");
    commit(&fixture.seed, "remove old skill file");
    git(&fixture.seed, &["push"]);
    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan = plan_repository_update(&source, &probe, &before).expect("plan update");

    assert_eq!(plan.affected().updated, ["demo"]);
    assert!(plan.affected().removed.is_empty());
    apply_repository_update(&plan).expect("fast-forward");
    fixture
        .app
        .perform_effects(&[Effect::ScanInstallations])
        .expect("scan after update");
    let report = verify_repository_update(&plan, &before, fixture.app.inventory());
    assert!(report.is_verified(), "{:?}", report.failures());
    assert!(report.is_complete(), "{:?}", report.withheld());
}

#[test]
fn replacing_an_installed_skill_directory_with_a_file_is_disclosed_as_removal() {
    let mut fixture = fixture();
    let root = fixture._temporary.path().join("home/.claude/skills");
    std::fs::create_dir_all(&root).expect("agent root");
    std::os::unix::fs::symlink(fixture.clone.join("skills/demo"), root.join("demo"))
        .expect("installed skill");
    fixture
        .app
        .perform_effects(&[Effect::ScanInstallations])
        .expect("scan before replacement");

    std::fs::remove_dir_all(fixture.seed.join("skills/demo")).expect("remove skill directory");
    std::fs::write(fixture.seed.join("skills/demo"), "not a skill directory\n")
        .expect("replacement file");
    commit(&fixture.seed, "replace skill directory with file");
    git(&fixture.seed, &["push"]);
    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan =
        plan_repository_update(&source, &probe, fixture.app.inventory()).expect("plan replacement");

    assert!(plan.affected().updated.is_empty());
    assert_eq!(plan.affected().removed, ["demo"]);
}

#[test]
fn deleting_an_uninstalled_sibling_edition_does_not_disclose_or_verify_a_removal() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let repository = temporary.path().join("library");
    Command::new("git")
        .args(["init", "-b", "main"])
        .arg(&repository)
        .output()
        .expect("initialize repository");
    for catalog in [".claude/skills", ".agents/skills"] {
        let skill = repository.join(catalog).join("demo");
        std::fs::create_dir_all(&skill).expect("edition directory");
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: demo\ndescription: fixture\n---\n",
        )
        .expect("edition skill");
    }
    commit(&repository, "two editions");
    git(&repository, &["switch", "-c", "upstream"]);
    std::fs::remove_dir_all(repository.join(".claude/skills/demo"))
        .expect("remove uninstalled Claude edition");
    commit(&repository, "remove Claude edition");
    git(&repository, &["switch", "main"]);
    git(
        &repository,
        &["branch", "--set-upstream-to=upstream", "main"],
    );
    let home = temporary.path().join("home");
    let mut app = SkilledApp::open(AppEnvironment::new(
        &home,
        temporary.path().join("data"),
        "",
    ))
    .expect("open app");
    let preview = app.preview_source(&repository).expect("preview editions");
    app.confirm_source(preview).expect("register editions");
    let codex_root = home.join(".agents/skills");
    std::fs::create_dir_all(&codex_root).expect("Codex root");
    std::os::unix::fs::symlink(
        repository.join(".agents/skills/demo"),
        codex_root.join("demo"),
    )
    .expect("install Codex edition");
    app.perform_effects(&[Effect::ScanInstallations])
        .expect("scan before update");
    let before = app.inventory().clone();

    let source = app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan = plan_repository_update(&source, &probe, &before).expect("plan edition update");

    assert!(plan.affected().updated.is_empty());
    assert!(plan.affected().removed.is_empty());
    assert!(plan.affected().renamed.is_empty());
    apply_repository_update(&plan).expect("fast-forward");
    app.perform_effects(&[Effect::ScanInstallations])
        .expect("scan after update");
    let report = verify_repository_update(&plan, &before, app.inventory());
    assert!(report.is_verified(), "{:?}", report.failures());
    assert!(report.is_complete(), "{:?}", report.withheld());
}

#[test]
fn advancing_a_submodule_that_contains_a_registered_catalog_is_blocked() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let library = temporary.path().join("library");
    Command::new("git")
        .args(["init", "-b", "main"])
        .arg(&library)
        .output()
        .expect("initialize library");
    let skill = library.join(".claude/skills/demo");
    std::fs::create_dir_all(&skill).expect("catalog skill");
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: demo\ndescription: fixture\n---\n",
    )
    .expect("skill");
    commit(&library, "initial library");

    let parent = temporary.path().join("parent");
    Command::new("git")
        .args(["init", "-b", "main"])
        .arg(&parent)
        .output()
        .expect("initialize parent");
    git(
        &parent,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            library.to_str().expect("library path"),
            "vendor/library",
        ],
    );
    commit(&parent, "register library submodule");
    git(&parent, &["branch", "upstream"]);

    std::fs::write(library.join(".claude/skills/demo/new.txt"), "incoming\n")
        .expect("library update");
    commit(&library, "update library");
    let target = git(&library, &["rev-parse", "HEAD"]);
    git(&parent, &["switch", "upstream"]);
    git(&parent.join("vendor/library"), &["fetch", "origin"]);
    git(&parent.join("vendor/library"), &["checkout", &target]);
    commit(&parent, "advance library submodule");
    git(&parent, &["switch", "main"]);
    git(
        &parent,
        &["-c", "protocol.file.allow=always", "submodule", "update"],
    );
    git(&parent, &["branch", "--set-upstream-to=upstream", "main"]);

    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open app");
    let preview = app.preview_source(&parent).expect("preview parent");
    app.confirm_source(preview).expect("register parent");
    let source = app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan = plan_repository_update(&source, &probe, app.inventory()).expect("plan update");

    assert!(plan.is_blocked());
    assert!(plan.findings().iter().any(|finding| {
        finding.code() == "source.submodule_update_unsupported"
            && finding.evidence().contains("vendor/library")
    }));
}

#[test]
fn deleting_a_file_from_a_root_skill_does_not_classify_the_root_as_removed() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let remote = temporary.path().join("remote.git");
    let seed = temporary.path().join("root-skill");
    let clone = temporary.path().join("root-clone");
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
    std::fs::write(
        seed.join("SKILL.md"),
        "---\nname: root-clone\ndescription: fixture\n---\n",
    )
    .expect("root skill");
    std::fs::write(seed.join("README.md"), "remove me\n").expect("readme");
    commit(&seed, "initial root skill");
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
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open app");
    let preview = app.preview_source(&clone).expect("preview root skill");
    app.confirm_source(preview).expect("register root skill");
    let root = temporary.path().join("home/.claude/skills");
    std::fs::create_dir_all(&root).expect("agent root");
    std::os::unix::fs::symlink(&clone, root.join("root-clone")).expect("installed root skill");
    app.perform_effects(&[Effect::ScanInstallations])
        .expect("scan installations");
    std::fs::remove_file(seed.join("README.md")).expect("remove readme");
    commit(&seed, "remove root readme");
    git(&seed, &["push"]);

    let source = app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan = plan_repository_update(&source, &probe, app.inventory()).expect("plan root update");

    assert_eq!(plan.affected().updated, ["root-clone"]);
    assert!(plan.affected().removed.is_empty());
}

#[test]
fn an_ambiguous_directory_move_discloses_removal_and_addition() {
    let mut fixture = fixture();
    let root = fixture._temporary.path().join("home/.claude/skills");
    std::fs::create_dir_all(&root).expect("agent root");
    std::os::unix::fs::symlink(fixture.clone.join("skills/demo"), root.join("demo"))
        .expect("installed skill");
    fixture
        .app
        .perform_effects(&[Effect::ScanInstallations])
        .expect("scan before update");

    std::fs::rename(
        fixture.seed.join("skills/demo"),
        fixture.seed.join("skills/renamed"),
    )
    .expect("rename skill directory");
    std::fs::write(
        fixture.seed.join("skills/renamed/SKILL.md"),
        "---\nname: renamed\ndescription: fixture\n---\n",
    )
    .expect("rename portable metadata");
    commit(&fixture.seed, "rename installed skill");
    git(&fixture.seed, &["push"]);

    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan =
        plan_repository_update(&source, &probe, fixture.app.inventory()).expect("plan rename");

    assert!(plan.affected().renamed.is_empty());
    assert_eq!(plan.affected().removed, ["demo"]);
    assert_eq!(plan.affected().added, ["renamed"]);
}

#[test]
fn an_exact_directory_move_is_reported_as_a_rename() {
    let mut fixture = fixture();
    let root = fixture._temporary.path().join("home/.claude/skills");
    std::fs::create_dir_all(&root).expect("agent root");
    std::os::unix::fs::symlink(fixture.clone.join("skills/demo"), root.join("demo"))
        .expect("installed skill");
    fixture
        .app
        .perform_effects(&[Effect::ScanInstallations])
        .expect("scan before update");

    std::fs::rename(
        fixture.seed.join("skills/demo"),
        fixture.seed.join("skills/renamed"),
    )
    .expect("rename skill directory");
    commit(&fixture.seed, "move installed skill exactly");
    git(&fixture.seed, &["push"]);

    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan =
        plan_repository_update(&source, &probe, fixture.app.inventory()).expect("plan rename");

    assert_eq!(
        plan.affected().renamed,
        [("demo".to_owned(), "renamed".to_owned())]
    );
    assert!(plan.affected().removed.is_empty());
    assert!(plan.affected().added.is_empty());
}

#[test]
fn an_undisclosed_post_merge_health_regression_fails_verification() {
    use std::os::unix::fs::PermissionsExt;

    let mut fixture = fixture();
    let root = fixture._temporary.path().join("home/.claude/skills");
    std::fs::create_dir_all(&root).expect("agent root");
    for name in ["demo", "other"] {
        std::os::unix::fs::symlink(fixture.clone.join("skills").join(name), root.join(name))
            .expect("installed skill");
    }
    fixture
        .app
        .perform_effects(&[Effect::ScanInstallations])
        .expect("scan before update");
    let before = fixture.app.inventory().clone();

    push_update(&fixture, "skills/demo/new.txt");
    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan = plan_repository_update(&source, &probe, &before).expect("plan update");
    assert_eq!(plan.affected().updated, ["demo"]);
    assert!(plan.affected().removed.is_empty());

    let hook = fixture.clone.join(".git/hooks/post-merge");
    std::fs::write(&hook, "#!/bin/sh\nrm -rf skills/other\n").expect("post-merge hook");
    let mut permissions = std::fs::metadata(&hook)
        .expect("hook metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&hook, permissions).expect("executable hook");

    apply_repository_update(&plan).expect("fast-forward");
    fixture
        .app
        .perform_effects(&[Effect::ScanInstallations])
        .expect("scan after update");
    let report = verify_repository_update(&plan, &before, fixture.app.inventory());

    assert!(!report.is_verified());
    assert!(report.is_complete());
    assert!(
        report
            .failures()
            .iter()
            .any(|failure| failure.contains("other") && failure.contains("regressed")),
        "{:?}",
        report.failures()
    );
}

/// A fast-forward writes inside the repository and nowhere near an agent root,
/// so an installation that exists only afterwards was disclosed by nothing —
/// and being healthy, it has no finding for the finding pass to catch either.
#[test]
fn an_undisclosed_installation_created_after_the_merge_fails_verification() {
    use std::os::unix::fs::PermissionsExt;

    let mut fixture = fixture();
    let root = fixture._temporary.path().join("home/.claude/skills");
    std::fs::create_dir_all(&root).expect("agent root");
    std::os::unix::fs::symlink(fixture.clone.join("skills/demo"), root.join("demo"))
        .expect("installed skill");
    fixture
        .app
        .perform_effects(&[Effect::ScanInstallations])
        .expect("scan before update");
    let before = fixture.app.inventory().clone();

    push_update(&fixture, "skills/demo/new.txt");
    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan = plan_repository_update(&source, &probe, &before).expect("plan update");
    assert!(plan.affected().added.is_empty());

    let hook = fixture.clone.join(".git/hooks/post-merge");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\nln -s '{}' '{}'\n",
            fixture.clone.join("skills/other").display(),
            root.join("other").display()
        ),
    )
    .expect("post-merge hook");
    let mut permissions = std::fs::metadata(&hook)
        .expect("hook metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&hook, permissions).expect("executable hook");

    apply_repository_update(&plan).expect("fast-forward");
    fixture
        .app
        .perform_effects(&[Effect::ScanInstallations])
        .expect("scan after update");
    let report = verify_repository_update(&plan, &before, fixture.app.inventory());

    assert!(!report.is_verified());
    assert!(report.is_complete());
    assert!(
        report
            .failures()
            .iter()
            .any(|failure| failure.contains("other") && failure.contains("appeared")),
        "{:?}",
        report.failures()
    );
}

#[test]
fn a_post_merge_hook_that_dirties_an_unrelated_tracked_file_fails_verification() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = fixture();
    std::fs::write(fixture.seed.join("tracked.txt"), "before\n").expect("tracked fixture");
    commit(&fixture.seed, "add tracked fixture");
    git(&fixture.seed, &["push"]);
    git(&fixture.clone, &["pull", "--ff-only"]);
    push_update(&fixture, "skills/demo/new.txt");
    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan =
        plan_repository_update(&source, &probe, fixture.app.inventory()).expect("plan update");
    let hook = fixture.clone.join(".git/hooks/post-merge");
    std::fs::write(&hook, "#!/bin/sh\nprintf 'hooked\\n' > tracked.txt\n")
        .expect("post-merge hook");
    let mut permissions = std::fs::metadata(&hook)
        .expect("hook metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&hook, permissions).expect("executable hook");

    apply_repository_update(&plan).expect("fast-forward");
    let report = verify_repository_update(&plan, fixture.app.inventory(), fixture.app.inventory());

    assert!(!report.is_verified());
    assert!(
        report
            .failures()
            .iter()
            .any(|failure| failure.contains("tracked changes")),
        "{:?}",
        report.failures()
    );
}

#[test]
fn a_hook_regression_to_an_unmanaged_installation_fails_verification() {
    use std::os::unix::fs::PermissionsExt;

    let mut fixture = fixture();
    let installation = fixture
        ._temporary
        .path()
        .join("home/.agents/skills/unmanaged");
    std::fs::create_dir_all(&installation).expect("unmanaged installation");
    std::fs::write(
        installation.join("SKILL.md"),
        "---\nname: unmanaged\ndescription: fixture\n---\n",
    )
    .expect("unmanaged skill");
    fixture
        .app
        .perform_effects(&[Effect::ScanInstallations])
        .expect("scan before update");
    let before = fixture.app.inventory().clone();

    push_update(&fixture, "skills/demo/new.txt");
    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan = plan_repository_update(&source, &probe, &before).expect("plan update");
    let hook = fixture.clone.join(".git/hooks/post-merge");
    std::fs::write(
        &hook,
        format!("#!/bin/sh\nrm -rf -- '{}'\n", installation.display()),
    )
    .expect("post-merge hook");
    let mut permissions = std::fs::metadata(&hook)
        .expect("hook metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&hook, permissions).expect("executable hook");

    apply_repository_update(&plan).expect("fast-forward");
    fixture
        .app
        .perform_effects(&[Effect::ScanInstallations])
        .expect("scan after update");
    let report = verify_repository_update(&plan, &before, fixture.app.inventory());

    assert!(!report.is_verified());
    assert!(report.failures().iter().any(|failure| {
        failure.contains("unmanaged") && failure.contains("disappeared without disclosure")
    }));
}

#[test]
fn a_hook_created_broken_installation_fails_verification() {
    use std::os::unix::fs::PermissionsExt;

    let mut fixture = fixture();
    let root = fixture._temporary.path().join("home/.agents/skills");
    std::fs::create_dir_all(&root).expect("agent root");
    fixture
        .app
        .perform_effects(&[Effect::ScanInstallations])
        .expect("scan before update");
    let before = fixture.app.inventory().clone();
    push_update(&fixture, "skills/demo/new.txt");
    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan = plan_repository_update(&source, &probe, &before).expect("plan update");
    let hook = fixture.clone.join(".git/hooks/post-merge");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\nln -s '{}' '{}'\n",
            fixture._temporary.path().join("missing").display(),
            root.join("new-broken").display()
        ),
    )
    .expect("post-merge hook");
    let mut permissions = std::fs::metadata(&hook)
        .expect("hook metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&hook, permissions).expect("executable hook");

    apply_repository_update(&plan).expect("fast-forward");
    fixture
        .app
        .perform_effects(&[Effect::ScanInstallations])
        .expect("scan after update");
    let report = verify_repository_update(&plan, &before, fixture.app.inventory());

    assert!(!report.is_verified());
    assert!(report.failures().iter().any(|failure| {
        failure.contains("new-broken") && failure.contains("install.dangling_symlink")
    }));
}

#[test]
fn verification_rejects_the_target_revision_on_another_branch() {
    let fixture = fixture();
    push_update(&fixture, "skills/demo/new.txt");
    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan =
        plan_repository_update(&source, &probe, fixture.app.inventory()).expect("plan update");
    apply_repository_update(&plan).expect("fast-forward");
    git(&fixture.clone, &["switch", "-c", "other"]);

    let report = verify_repository_update(&plan, fixture.app.inventory(), fixture.app.inventory());

    assert!(!report.is_verified());
    assert!(
        report
            .failures()
            .iter()
            .any(|failure| failure.contains("refs/heads/other")),
        "{:?}",
        report.failures()
    );
}

#[test]
fn changed_file_evidence_does_not_extend_the_confirmation_gate() {
    let mut fixture = fixture();
    for index in 0..100 {
        std::fs::write(
            fixture.seed.join(format!("evidence-{index:03}.txt")),
            "incoming\n",
        )
        .expect("evidence file");
    }
    commit(&fixture.seed, "large upstream change");
    git(&fixture.seed, &["push"]);
    fixture
        .app
        .perform_effects(&[Effect::CheckUpdates])
        .expect("start check");
    finish_update_check(&mut fixture.app);
    fixture
        .app
        .perform_effects(&[Effect::PlanRepositoryUpdate])
        .expect("plan update");

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut feedback = tui::RenderFeedback::default();
    terminal
        .draw(|frame| feedback = tui::render(frame, &fixture.app))
        .expect("render preview");
    assert!(
        feedback
            .detail_max_scroll()
            .is_some_and(|extent| extent > 50)
    );
    assert_eq!(feedback.update_preview_fully_seen(), Some(true));
    let buffer = terminal.backend().buffer();
    let footer = (buffer.area.x..buffer.area.x + buffer.area.width)
        .map(|x| buffer[(x, buffer.area.y + buffer.area.height - 1)].symbol())
        .collect::<String>();
    assert!(footer.contains("Enter Apply"), "{footer}");
    fixture
        .app
        .note_detail_max_scroll(feedback.detail_max_scroll());
    fixture
        .app
        .note_update_preview_fully_seen(feedback.update_preview_fully_seen());

    let update = fixture.app.update(Action::ConfirmRepositoryUpdate);
    assert_eq!(update.effects(), &[Effect::ApplyRepositoryUpdate]);
}

#[test]
fn a_verified_apply_reports_when_its_cached_result_cannot_be_saved() {
    let mut fixture = fixture();
    push_update(&fixture, "skills/demo/new.txt");
    fixture
        .app
        .perform_effects(&[Effect::CheckUpdates])
        .expect("start check");
    finish_update_check(&mut fixture.app);
    fixture
        .app
        .perform_effects(&[Effect::PlanRepositoryUpdate])
        .expect("plan update");
    let connection =
        rusqlite::Connection::open(fixture._temporary.path().join("data/skilled.sqlite3"))
            .expect("open metadata");
    connection
        .execute_batch(
            "CREATE TRIGGER reject_update_check BEFORE UPDATE ON source_update_checks
             BEGIN SELECT RAISE(FAIL, 'fixture persistence failure'); END;",
        )
        .expect("install failure trigger");
    drop(connection);

    fixture
        .app
        .perform_effects(&[Effect::ApplyRepositoryUpdate])
        .expect("apply update");

    assert!(matches!(
        fixture.app.pending_update(),
        Some(RepositoryUpdatePrompt::Report {
            verification,
            persistence_error: Some(error),
            ..
        }) if verification.is_verified() && error.contains("fixture persistence failure")
    ));
}

#[test]
fn a_failed_apply_refreshes_and_reports_the_post_attempt_state() {
    let mut fixture = fixture();
    push_update(&fixture, "skills/demo/new.txt");
    fixture
        .app
        .perform_effects(&[Effect::CheckUpdates])
        .expect("start check");
    finish_update_check(&mut fixture.app);
    fixture
        .app
        .perform_effects(&[Effect::PlanRepositoryUpdate])
        .expect("plan update");
    git(&fixture.clone, &["switch", "-c", "other"]);

    fixture
        .app
        .perform_effects(&[Effect::ApplyRepositoryUpdate])
        .expect("observe failed apply");

    assert_eq!(fixture.app.sources()[0].branch(), Some("other"));
    assert!(matches!(
        fixture.app.pending_update(),
        Some(RepositoryUpdatePrompt::Report {
            apply_error: Some(_),
            verification,
            ..
        }) if !verification.is_verified()
            && verification.failures().iter().any(|failure| failure.contains("other"))
    ));
    assert!(
        fixture.app.update_checks()[0]
            .findings()
            .iter()
            .all(|finding| finding.code() != "update.verification_failed")
    );
}

#[test]
fn a_guard_refusal_caches_the_current_blocker_not_a_failed_postcondition() {
    let mut fixture = fixture();
    push_update(&fixture, "skills/demo/new.txt");
    fixture
        .app
        .perform_effects(&[Effect::CheckUpdates])
        .expect("start check");
    finish_update_check(&mut fixture.app);
    fixture
        .app
        .perform_effects(&[Effect::PlanRepositoryUpdate])
        .expect("plan update");
    let checked_at = fixture.app.update_checks()[0].checked_at;
    std::fs::write(
        fixture.clone.join("skills/demo/old.txt"),
        "late local edit\n",
    )
    .expect("late edit");

    fixture
        .app
        .perform_effects(&[Effect::ApplyRepositoryUpdate])
        .expect("observe guard refusal");

    let findings = fixture.app.update_checks()[0].findings();
    assert_eq!(fixture.app.update_checks()[0].checked_at, checked_at);
    assert!(findings.iter().any(|finding| {
        finding.code() == "source.changed_after_preview"
            && finding.evidence().contains("check updates again")
    }));
    assert!(
        findings
            .iter()
            .all(|finding| finding.code() != "update.verification_failed"),
        "{findings:?}"
    );
}

#[test]
fn a_changed_checkout_still_fetches_during_an_explicit_check() {
    let fixture = fixture();
    std::fs::write(fixture.clone.join("local.txt"), "local\n").expect("local file");
    commit(&fixture.clone, "local change");
    let target = push_update(&fixture, "skills/demo/new.txt");

    let probe = probe_repository_update(&fixture.app.sources()[0], true);

    assert_eq!(
        probe.upstream.as_ref().map(|upstream| upstream.revision()),
        Some(target.as_str())
    );
    assert_eq!(probe.behind, 1);
    assert_eq!(probe.ahead, 1);
}

#[test]
fn configured_squash_merge_options_cannot_change_fast_forward_semantics() {
    let fixture = fixture();
    let target = push_update(&fixture, "skills/demo/new.txt");
    git(
        &fixture.clone,
        &["config", "branch.main.mergeOptions", "--squash"],
    );
    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan =
        plan_repository_update(&source, &probe, fixture.app.inventory()).expect("plan update");

    apply_repository_update(&plan).expect("fast-forward despite configured squash mode");

    assert_eq!(git(&fixture.clone, &["rev-parse", "HEAD"]), target);
    assert_eq!(git(&fixture.clone, &["status", "--porcelain"]), "");
}

#[test]
fn a_replaced_checkout_is_refused_before_an_explicit_check_fetches() {
    let fixture = fixture();
    let attacker = fixture._temporary.path().join("attacker");
    Command::new("git")
        .args(["clone", "--branch", "main"])
        .arg(&fixture.remote)
        .arg(&attacker)
        .output()
        .expect("attacker clone");
    let attacker_tracking_before = git(&attacker, &["rev-parse", "refs/remotes/origin/main"]);
    push_update(&fixture, "skills/demo/new.txt");
    let displaced = fixture._temporary.path().join("displaced-clone");
    std::fs::rename(&fixture.clone, &displaced).expect("displace registered checkout");
    std::os::unix::fs::symlink(&attacker, &fixture.clone).expect("replace checkout with symlink");

    let probe = probe_repository_update(&fixture.app.sources()[0], true);
    let (verdict, findings) = classify_repository_update(&probe);

    assert_eq!(verdict, RepositoryUpdateVerdict::Blocked);
    assert!(
        findings
            .iter()
            .any(|finding| finding.code() == "source.missing")
    );
    assert_eq!(
        git(&attacker, &["rev-parse", "refs/remotes/origin/main"]),
        attacker_tracking_before
    );
}

#[test]
fn a_different_checkout_at_the_registered_path_is_refused_before_fetch() {
    let fixture = fixture();
    push_update(&fixture, "skills/demo/new.txt");
    let displaced = fixture._temporary.path().join("displaced-clone");
    std::fs::rename(&fixture.clone, &displaced).expect("displace registered checkout");
    Command::new("git")
        .args(["clone", "--branch", "main"])
        .arg(&fixture.remote)
        .arg(&fixture.clone)
        .output()
        .expect("replacement clone");

    let probe = probe_repository_update(&fixture.app.sources()[0], true);

    assert!(
        probe
            .error
            .as_deref()
            .is_some_and(|error| error.starts_with("source.missing|")),
        "{:?}",
        probe.error
    );
}

#[test]
fn a_different_checkout_at_the_registered_path_cannot_be_fast_forwarded() {
    let fixture = fixture();
    let original = git(&fixture.clone, &["rev-parse", "HEAD"]);
    push_update(&fixture, "skills/demo/new.txt");
    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let plan =
        plan_repository_update(&source, &probe, fixture.app.inventory()).expect("plan update");
    let displaced = fixture._temporary.path().join("displaced-clone");
    std::fs::rename(&fixture.clone, &displaced).expect("displace registered checkout");
    Command::new("git")
        .args(["clone", "--branch", "main"])
        .arg(&fixture.remote)
        .arg(&fixture.clone)
        .output()
        .expect("replacement clone");
    git(&fixture.clone, &["reset", "--hard", &original]);

    assert!(apply_repository_update(&plan).is_err());
    assert_eq!(git(&fixture.clone, &["rev-parse", "HEAD"]), original);
}

#[test]
fn a_replaced_checkout_is_still_refused_after_restart() {
    let fixture = fixture();
    let original = git(&fixture.clone, &["rev-parse", "HEAD"]);
    drop(fixture.app);
    let displaced = fixture._temporary.path().join("displaced-clone");
    std::fs::rename(&fixture.clone, &displaced).expect("displace registered checkout");
    Command::new("git")
        .args(["clone", "--branch", "main"])
        .arg(&fixture.remote)
        .arg(&fixture.clone)
        .output()
        .expect("replacement clone");
    assert_eq!(git(&fixture.clone, &["rev-parse", "HEAD"]), original);

    let reopened = SkilledApp::open(AppEnvironment::new(
        fixture._temporary.path().join("home"),
        fixture._temporary.path().join("data"),
        "",
    ))
    .expect("reopen app");

    assert!(
        reopened.sources()[0]
            .source_error()
            .is_some_and(|error| error.contains("different Git checkout"))
    );
    let probe = probe_repository_update(&reopened.sources()[0], true);
    assert!(probe.error.is_some());
}

#[test]
fn an_upstream_mapped_into_a_local_branch_is_refused_before_fetch() {
    let fixture = fixture();
    git(
        &fixture.clone,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/main:refs/heads/local-upstream",
        ],
    );
    git(&fixture.clone, &["fetch", "origin"]);
    let local_upstream = git(&fixture.clone, &["rev-parse", "refs/heads/local-upstream"]);
    push_update(&fixture, "skills/demo/new.txt");

    let probe = probe_repository_update(&fixture.app.sources()[0], true);

    assert!(probe.error.is_some(), "unsafe destination was accepted");
    assert_eq!(
        git(&fixture.clone, &["rev-parse", "refs/heads/local-upstream"]),
        local_upstream
    );
}

#[test]
fn a_local_dot_upstream_is_a_valid_non_network_update_source() {
    let fixture = fixture();
    git(&fixture.clone, &["branch", "upstream"]);
    git(
        &fixture.clone,
        &["branch", "--set-upstream-to=upstream", "main"],
    );
    git(&fixture.clone, &["switch", "upstream"]);
    std::fs::write(fixture.clone.join("local-upstream.txt"), "ahead\n").expect("local update");
    commit(&fixture.clone, "local upstream update");
    git(&fixture.clone, &["switch", "main"]);
    drop(fixture.app);
    let app = SkilledApp::open(AppEnvironment::new(
        fixture._temporary.path().join("home"),
        fixture._temporary.path().join("data"),
        "",
    ))
    .expect("refresh registered source");

    let probe = probe_repository_update(&app.sources()[0], true);
    let (verdict, findings) = classify_repository_update(&probe);

    assert_eq!(verdict, RepositoryUpdateVerdict::Available, "{findings:?}");
    assert!(findings.is_empty());
    assert_eq!(
        probe.upstream.as_ref().map(|upstream| upstream.remote()),
        Some(".")
    );
}

#[test]
fn tui_checks_run_off_the_event_loop_and_quit_cancels() {
    let mut fixture = fixture();
    let started = Instant::now();
    fixture
        .app
        .perform_effects(&[Effect::CheckUpdates])
        .expect("start worker");
    assert!(started.elapsed() < Duration::from_millis(100));
    assert!(fixture.app.update_check_in_flight());
    assert_eq!(
        fixture.app.update(Action::Quit).effects(),
        &[Effect::CancelUpdateCheck]
    );
    fixture
        .app
        .perform_effects(&[Effect::CancelUpdateCheck])
        .expect("cancel worker");
    // A cancelled run retains ownership until every Git query has stopped, so
    // another check cannot overlap a slow non-fetch inspection.
    finish_update_check(&mut fixture.app);
    assert!(fixture.app.update_checks().is_empty());
}

#[test]
fn updates_footer_offers_enter_only_when_the_action_can_run() {
    fn footer(app: &SkilledApp) -> String {
        let backend = TestBackend::new(170, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                tui::render(frame, app);
            })
            .expect("render updates");
        let buffer = terminal.backend().buffer();
        (buffer.area.x..buffer.area.x + buffer.area.width)
            .map(|x| buffer[(x, buffer.area.y + buffer.area.height - 1)].symbol())
            .collect()
    }

    let mut fixture = fixture();
    for _ in 0..7 {
        let update = fixture.app.update(Action::Continue);
        fixture
            .app
            .perform_effects(update.effects())
            .expect("complete setup");
    }
    fixture.app.update(Action::OpenUpdates);
    assert!(footer(&fixture.app).contains("Enter Open"));
    fixture
        .app
        .perform_effects(&[Effect::CheckUpdates])
        .expect("start check");
    assert!(!footer(&fixture.app).contains("Enter Open"));
    finish_update_check(&mut fixture.app);
    fixture.app.update(Action::AdvanceUpdatesPane);
    assert!(!footer(&fixture.app).contains("Enter Open"));
}

#[test]
fn an_active_check_renders_determinate_segmented_progress() {
    let mut fixture = fixture();
    for _ in 0..7 {
        let update = fixture.app.update(Action::Continue);
        fixture
            .app
            .perform_effects(update.effects())
            .expect("complete setup");
    }
    fixture.app.update(Action::OpenUpdates);
    fixture
        .app
        .perform_effects(&[Effect::CheckUpdates])
        .expect("start check");
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| {
            tui::render(frame, &fixture.app);
        })
        .expect("render progress");
    let buffer = terminal.backend().buffer();
    let rendered = (buffer.area.y..buffer.area.y + buffer.area.height)
        .map(|y| {
            (buffer.area.x..buffer.area.x + buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(rendered.contains('●'), "{rendered}");
    fixture
        .app
        .perform_effects(&[Effect::CancelUpdateCheck])
        .expect("cancel check");
    finish_update_check(&mut fixture.app);
}

#[test]
fn a_finished_tui_check_persists_its_cached_result() {
    let mut fixture = fixture();
    push_update(&fixture, "skills/demo/new.txt");
    fixture
        .app
        .perform_effects(&[Effect::CheckUpdates])
        .expect("start worker");
    finish_update_check(&mut fixture.app);
    assert_eq!(fixture.app.update_checks().len(), 1);
    assert_eq!(
        fixture.app.update_checks()[0].verdict,
        RepositoryUpdateVerdict::Available
    );
}

#[test]
fn fetching_an_update_advances_the_configured_remote_tracking_ref() {
    let fixture = fixture();
    let target = push_update(&fixture, "skills/demo/new.txt");
    let source = fixture.app.sources()[0].clone();

    let probe = probe_repository_update(&source, true);

    assert!(probe.error.is_none(), "{:?}", probe.error);
    assert_eq!(
        git(&fixture.clone, &["rev-parse", "refs/remotes/origin/main"]),
        target
    );
}

#[test]
fn a_fresh_check_uses_the_same_dirtiness_observation_for_supersession() {
    let mut fixture = fixture();
    push_update(&fixture, "skills/demo/new.txt");
    std::fs::write(fixture.clone.join("local-untracked.txt"), "local\n").expect("untracked file");
    fixture
        .app
        .perform_effects(&[Effect::CheckUpdates])
        .expect("start worker");
    finish_update_check(&mut fixture.app);

    let source = &fixture.app.sources()[0];
    let check = &fixture.app.update_checks()[0];
    assert_eq!(source.dirty(), Some(true));
    assert!(check.dirty);
    assert!(!check.superseded_by(source));
    assert_eq!(fixture.app.stated_update_count(), Some(1));
}

#[test]
fn a_detached_check_preserves_detached_and_no_upstream_findings() {
    let mut fixture = fixture();
    git(&fixture.clone, &["checkout", "--detach"]);
    fixture
        .app
        .perform_effects(&[Effect::CheckUpdates])
        .expect("start worker");
    finish_update_check(&mut fixture.app);

    let codes = fixture
        .app
        .doctor_findings()
        .into_iter()
        .map(|item| item.finding().code())
        .collect::<Vec<_>>();
    assert!(codes.contains(&"source.detached_head"), "{codes:?}");
    assert!(codes.contains(&"source.no_upstream"), "{codes:?}");
}

#[test]
fn a_cached_check_is_superseded_when_the_checkout_becomes_unavailable() {
    let mut fixture = fixture();
    push_update(&fixture, "skills/demo/new.txt");
    fixture
        .app
        .perform_effects(&[Effect::CheckUpdates])
        .expect("start worker");
    finish_update_check(&mut fixture.app);
    drop(fixture.app);
    std::fs::rename(
        &fixture.clone,
        fixture._temporary.path().join("moved-clone"),
    )
    .expect("move checkout");

    let reopened = SkilledApp::open(AppEnvironment::new(
        fixture._temporary.path().join("home"),
        fixture._temporary.path().join("data"),
        "",
    ))
    .expect("reopen app");
    let source = &reopened.sources()[0];
    let check = &reopened.update_checks()[0];

    assert!(source.source_error().is_some());
    assert!(check.superseded_by(source));
    assert_eq!(reopened.stated_update_count(), None);
}

#[test]
fn detaching_at_the_same_commit_supersedes_an_attached_cached_check() {
    let mut fixture = fixture();
    push_update(&fixture, "skills/demo/new.txt");
    fixture
        .app
        .perform_effects(&[Effect::CheckUpdates])
        .expect("start worker");
    finish_update_check(&mut fixture.app);
    git(&fixture.clone, &["checkout", "--detach"]);
    drop(fixture.app);

    let reopened = SkilledApp::open(AppEnvironment::new(
        fixture._temporary.path().join("home"),
        fixture._temporary.path().join("data"),
        "",
    ))
    .expect("reopen app");

    assert!(reopened.update_checks()[0].superseded_by(&reopened.sources()[0]));
    assert_eq!(reopened.stated_update_count(), None);
}

#[test]
fn switching_branches_at_the_same_commit_supersedes_the_cached_check() {
    let mut fixture = fixture();
    push_update(&fixture, "skills/demo/new.txt");
    fixture
        .app
        .perform_effects(&[Effect::CheckUpdates])
        .expect("start check");
    finish_update_check(&mut fixture.app);
    git(&fixture.clone, &["switch", "-c", "other"]);
    drop(fixture.app);

    let reopened = SkilledApp::open(AppEnvironment::new(
        fixture._temporary.path().join("home"),
        fixture._temporary.path().join("data"),
        "",
    ))
    .expect("reopen app");

    assert!(reopened.update_checks()[0].superseded_by(&reopened.sources()[0]));
    assert_eq!(reopened.stated_update_count(), None);
}

#[test]
fn moving_the_fetched_tracking_ref_before_preview_blocks_the_stale_plan() {
    let mut fixture = fixture();
    push_update(&fixture, "skills/demo/first.txt");
    fixture
        .app
        .perform_effects(&[Effect::CheckUpdates])
        .expect("start check");
    finish_update_check(&mut fixture.app);
    push_update(&fixture, "skills/demo/second.txt");
    git(&fixture.clone, &["fetch", "origin"]);

    fixture
        .app
        .perform_effects(&[Effect::PlanRepositoryUpdate])
        .expect("build stale preview");

    assert!(matches!(
        fixture.app.pending_update(),
        Some(RepositoryUpdatePrompt::Preview(plan)) if plan.is_blocked()
            && plan.findings().iter().any(|finding| {
                finding.code() == "source.changed_after_preview"
                    && !finding.evidence().contains("fetch")
            })
    ));
}

#[test]
fn a_fresh_missing_source_check_is_current_and_visible_in_doctor() {
    let fixture = fixture();
    let moved = fixture._temporary.path().join("moved-clone");
    std::fs::rename(&fixture.clone, &moved).expect("move checkout");
    drop(fixture.app);
    let mut app = SkilledApp::open(AppEnvironment::new(
        fixture._temporary.path().join("home"),
        fixture._temporary.path().join("data"),
        "",
    ))
    .expect("reopen app");
    app.perform_effects(&[Effect::CheckUpdates])
        .expect("start missing check");
    finish_update_check(&mut app);

    assert!(!app.update_checks()[0].superseded_by(&app.sources()[0]));
    assert!(
        app.doctor_findings()
            .iter()
            .any(|entry| { entry.finding().code() == "source.missing" })
    );
    assert_eq!(app.stated_update_count(), None);
}

/// Doctor promises every finding an account of what it costs the reader, and a
/// repository finding is a finding like any other: it may not fall through to
/// the sentence that says Skilled has none.
#[test]
fn a_repository_finding_states_its_consequence_in_doctor() {
    let mut fixture = fixture();
    push_update(&fixture, "skills/demo/new.txt");
    std::fs::write(fixture.clone.join("skills/demo/old.txt"), "local edit\n").expect("local edit");
    fixture
        .app
        .perform_effects(&[Effect::CheckUpdates])
        .expect("start check");
    finish_update_check(&mut fixture.app);
    {
        let source = &fixture.app.sources()[0];
        let checks = fixture.app.update_checks();
        eprintln!("DIAG git status --porcelain=v1:");
        eprintln!("{}", git(&fixture.clone, &["status", "--porcelain=v1"]));
        eprintln!(
            "DIAG source: head={} branch={:?} dirty={:?} error={:?}",
            source.head(),
            source.branch(),
            source.dirty(),
            source.source_error()
        );
        for check in checks {
            eprintln!(
                "DIAG check: revision={} reference={:?} dirty={} dirty_known={} verdict={:?} superseded={} findings={:?}",
                check.local_revision,
                check.local_reference,
                check.dirty,
                check.dirty_known,
                check.verdict,
                check.superseded_by(source),
                check
                    .findings()
                    .iter()
                    .map(|finding| finding.code().to_owned())
                    .collect::<Vec<_>>()
            );
        }
        eprintln!(
            "DIAG doctor: {:?}",
            fixture
                .app
                .doctor_findings()
                .iter()
                .map(|entry| entry.finding().code().to_owned())
                .collect::<Vec<_>>()
        );
    }
    assert!(
        fixture
            .app
            .doctor_findings()
            .iter()
            .any(|entry| entry.finding().code() == "source.dirty")
    );

    for _ in 0..7 {
        let update = fixture.app.update(Action::Continue);
        fixture
            .app
            .perform_effects(update.effects())
            .expect("complete setup");
    }
    fixture.app.update(Action::OpenDoctor);
    let position = fixture
        .app
        .doctor_findings()
        .iter()
        .position(|entry| entry.finding().code() == "source.dirty")
        .expect("a listed repository finding");
    for _ in 0..position {
        fixture.app.update(Action::MoveDoctorSelection(1));
    }
    fixture.app.update(Action::AdvanceDoctorPane);
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| {
            tui::render(frame, &fixture.app);
        })
        .expect("render doctor");
    let buffer = terminal.backend().buffer();
    let rendered = (buffer.area.y..buffer.area.y + buffer.area.height)
        .map(|y| {
            (buffer.area.x..buffer.area.x + buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        !rendered.contains("has no account of what this costs"),
        "{rendered}"
    );
    assert!(rendered.contains("changes of its own"), "{rendered}");
}

#[test]
fn a_failed_fetch_withholds_the_whole_registry_update_count() {
    let mut fixture = fixture();
    git(
        &fixture.clone,
        &["remote", "set-url", "origin", "/missing/remote"],
    );
    fixture
        .app
        .perform_effects(&[Effect::CheckUpdates])
        .expect("start failed check");
    finish_update_check(&mut fixture.app);

    assert_eq!(
        fixture.app.update_checks()[0].verdict,
        RepositoryUpdateVerdict::Blocked
    );
    assert!(!fixture.app.update_checks()[0].superseded_by(&fixture.app.sources()[0]));
    assert!(fixture.app.doctor_findings().iter().any(|item| {
        item.finding().code() == "source.fetch_failed"
            && item
                .finding()
                .evidence()
                .contains("Git command exited with status")
    }));
    assert_eq!(fixture.app.stated_update_count(), None);
}

#[test]
fn a_rewritten_upstream_is_diverged_at_the_explicitly_fetched_tracking_ref() {
    let fixture = fixture();
    let rewritten = fixture._temporary.path().join("rewritten");
    Command::new("git")
        .args(["init", "-b", "main"])
        .arg(&rewritten)
        .output()
        .expect("rewritten repository");
    std::fs::write(rewritten.join("replacement.txt"), "replacement\n").expect("replacement");
    commit(&rewritten, "unrelated root");
    git(
        &rewritten,
        &["remote", "add", "origin", fixture.remote.to_str().unwrap()],
    );
    git(&rewritten, &["push", "--force", "origin", "main"]);

    let source = fixture.app.sources()[0].clone();
    let probe = probe_repository_update(&source, true);
    let (verdict, findings) = classify_repository_update(&probe);

    assert_eq!(verdict, RepositoryUpdateVerdict::Blocked);
    assert!(
        findings
            .iter()
            .any(|finding| finding.code() == "source.diverged"),
        "{findings:?}"
    );
    assert_eq!(
        git(&fixture.clone, &["rev-parse", "refs/remotes/origin/main"]),
        git(&rewritten, &["rev-parse", "HEAD"])
    );
}
