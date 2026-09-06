use skilled::{Action, AppEnvironment, SkilledApp, adoption::AdoptionPrompt};
use std::{fs, path::Path, process::Command};

fn dispatch(app: &mut SkilledApp, action: Action) {
    let result = app.update(action);
    app.perform_effects(result.effects()).unwrap();
}
fn git(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().into()
}
fn fixture() -> (tempfile::TempDir, SkilledApp) {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir_all(source.join("skills/demo")).unwrap();
    fs::write(
        source.join("skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: Fixture\n---\nBody\n",
    )
    .unwrap();
    git(&source, &["init", "-b", "main"]);
    git(&source, &["add", "."]);
    git(
        &source,
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
    let mut app = SkilledApp::open(AppEnvironment::new(
        temp.path().join("home"),
        temp.path().join("data"),
        "",
    ))
    .unwrap();
    app.confirm_source(app.preview_source(&source).unwrap())
        .unwrap();
    for _ in 0..7 {
        dispatch(&mut app, Action::Continue);
    }
    dispatch(&mut app, Action::OpenSources);
    dispatch(&mut app, Action::AdvanceSourcesPane);
    (temp, app)
}
fn fill(app: &mut SkilledApp) {
    dispatch(app, Action::BeginAdoption);
    assert!(matches!(
        app.pending_adoption(),
        Some(AdoptionPrompt::Editing(_))
    ));
    for field in [
        "https://github.com/example/upstream",
        "skills/demo",
        "refs/heads/main",
    ] {
        for c in field.chars() {
            dispatch(app, Action::AppendAdoptionCharacter(c));
        }
        dispatch(app, Action::NextAdoptionField);
    }
    dispatch(app, Action::PreviewAdoption);
    assert!(
        matches!(app.pending_adoption(), Some(AdoptionPrompt::Preview(_))),
        "{:?}",
        app.pending_adoption()
    );
}
fn count(temp: &tempfile::TempDir) -> i64 {
    rusqlite::Connection::open(temp.path().join("data/skilled.sqlite3"))
        .unwrap()
        .query_row("SELECT count(*) FROM origin_baselines", [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[test]
fn confirmation_requires_visible_preview_and_saves_an_honest_baseline() {
    let (temp, mut app) = fixture();
    let source = temp.path().join("source");
    let head = git(&source, &["rev-parse", "HEAD"]);
    fill(&mut app);
    assert!(app.update(Action::ConfirmAdoption).effects().is_empty());
    app.note_detail_max_scroll(Some(0));
    dispatch(&mut app, Action::ConfirmAdoption);
    assert!(
        matches!(app.pending_adoption(), Some(AdoptionPrompt::Report(_))),
        "{:?}",
        app.pending_adoption()
    );
    assert_eq!(count(&temp), 1);
    assert_eq!(git(&source, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&source, &["status", "--porcelain"]), "");
    dispatch(&mut app, Action::DismissAdoption);
    dispatch(&mut app, Action::BeginAdoption);
    assert!(
        matches!(app.pending_adoption(), Some(AdoptionPrompt::Failed(message)) if message.contains("already"))
    );
}

#[test]
fn changed_content_invalidates_preview_without_saving_metadata() {
    let (temp, mut app) = fixture();
    fill(&mut app);
    fs::write(
        temp.path().join("source/skills/demo/untracked.txt"),
        "new content",
    )
    .unwrap();
    app.note_detail_max_scroll(Some(0));
    dispatch(&mut app, Action::ConfirmAdoption);
    assert!(
        matches!(app.pending_adoption(), Some(AdoptionPrompt::Failed(message)) if message.contains("content changed") && !message.contains("saved"))
    );
    assert_eq!(count(&temp), 0);
}

#[test]
fn changed_attribution_and_catalog_registration_invalidate_preview() {
    for change_registration in [false, true] {
        let (temp, mut app) = fixture();
        fill(&mut app);
        if change_registration {
            let db = rusqlite::Connection::open(temp.path().join("data/skilled.sqlite3")).unwrap();
            db.execute("DELETE FROM catalog_roots", []).unwrap();
        } else {
            fs::write(temp.path().join("source/ATTRIBUTION.md"), "| Skill | Source | License |\n| `demo` | https://github.com/other/origin | MIT |\n").unwrap();
        }
        app.note_detail_max_scroll(Some(0));
        dispatch(&mut app, Action::ConfirmAdoption);
        assert!(matches!(
            app.pending_adoption(),
            Some(AdoptionPrompt::Failed(_))
        ));
        assert_eq!(count(&temp), 0);
    }
}

#[cfg(unix)]
#[test]
fn redirecting_a_skill_parent_invalidates_preview() {
    let (temp, mut app) = fixture();
    fill(&mut app);
    let source = temp.path().join("source");
    fs::rename(source.join("skills"), source.join("moved")).unwrap();
    std::os::unix::fs::symlink("moved", source.join("skills")).unwrap();
    app.note_detail_max_scroll(Some(0));
    dispatch(&mut app, Action::ConfirmAdoption);
    assert!(matches!(
        app.pending_adoption(),
        Some(AdoptionPrompt::Failed(_))
    ));
    assert_eq!(count(&temp), 0);
}

#[test]
fn metadata_write_failure_degrades_the_session_and_keeps_content() {
    let (temp, mut app) = fixture();
    fill(&mut app);
    let db = rusqlite::Connection::open(temp.path().join("data/skilled.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_adoption BEFORE INSERT ON origin_baselines BEGIN SELECT RAISE(FAIL, 'injected metadata failure'); END;").unwrap();
    app.note_detail_max_scroll(Some(0));
    dispatch(&mut app, Action::ConfirmAdoption);
    assert!(app.metadata_failure().is_some());
    assert!(!app.can_add_source());
    assert_eq!(count(&temp), 0);
    assert!(temp.path().join("source/skills/demo/SKILL.md").is_file());
}

#[test]
fn an_ambiguous_origin_requires_an_explicit_matching_choice() {
    let (temp, mut app) = fixture();
    fs::write(temp.path().join("source/ATTRIBUTION.md"), "| Skill | Source | License |\n| `demo` | https://github.com/example/one | MIT |\n| `demo` | https://github.com/example/two | MIT |\n").unwrap();
    dispatch(&mut app, Action::BeginAdoption);
    dispatch(&mut app, Action::PreviewAdoption);
    assert!(
        matches!(app.pending_adoption(), Some(AdoptionPrompt::Editing(draft)) if draft.error.is_some())
    );
    for field in [
        "https://github.com/example/two",
        "skills/demo",
        "refs/heads/main",
    ] {
        for c in field.chars() {
            dispatch(&mut app, Action::AppendAdoptionCharacter(c));
        }
        dispatch(&mut app, Action::NextAdoptionField);
    }
    dispatch(&mut app, Action::PreviewAdoption);
    assert!(
        matches!(app.pending_adoption(), Some(AdoptionPrompt::Preview(_))),
        "{:?}",
        app.pending_adoption()
    );
    app.note_detail_max_scroll(Some(0));
    dispatch(&mut app, Action::ConfirmAdoption);
    assert_eq!(count(&temp), 1);
}

#[test]
fn a_repository_root_skill_can_establish_a_baseline() {
    let (temp, mut app) = fixture();
    let source = temp.path().join("source");
    fs::write(
        source.join("SKILL.md"),
        "---\nname: source\ndescription: Root skill\n---\nRoot body\n",
    )
    .unwrap();
    app.confirm_source(app.preview_source(&source).unwrap())
        .unwrap();
    dispatch(&mut app, Action::AdvanceSourcesPane);
    assert!(app.can_adopt_selection());
    fill(&mut app);
    app.note_detail_max_scroll(Some(0));
    dispatch(&mut app, Action::ConfirmAdoption);
    assert!(
        matches!(app.pending_adoption(), Some(AdoptionPrompt::Report(_))),
        "{:?}",
        app.pending_adoption()
    );
    let db = rusqlite::Connection::open(temp.path().join("data/skilled.sqlite3")).unwrap();
    let record: (String, Option<String>, String) = db
        .query_row(
            "SELECT variant_relative_path, proven_revision, association FROM origin_baselines",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        record,
        (".".into(), None, "explicit-current-content".into())
    );
}

#[test]
fn a_changed_saved_record_is_reported_as_saved_and_degrades_metadata() {
    let (temp, mut app) = fixture();
    fill(&mut app);
    let db = rusqlite::Connection::open(temp.path().join("data/skilled.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER change_adoption AFTER INSERT ON origin_baselines BEGIN UPDATE origin_baselines SET baseline_digest = '0000000000000000000000000000000000000000000000000000000000000000'; END;").unwrap();
    app.note_detail_max_scroll(Some(0));
    dispatch(&mut app, Action::ConfirmAdoption);
    assert!(app.metadata_failure().is_some());
    assert!(
        matches!(app.pending_adoption(), Some(AdoptionPrompt::Failed(message)) if message.contains("Baseline saved") && message.contains("verification failed"))
    );
    assert_eq!(count(&temp), 1);
}

#[test]
fn changing_only_a_pinned_attribution_commit_invalidates_confirmation() {
    let (temp, mut app) = fixture();
    let attribution = temp.path().join("source/ATTRIBUTION.md");
    let first = "| Skill | Source | License |\n| `demo` | https://github.com/example/upstream/tree/1111111111111111111111111111111111111111/skills/demo | MIT |\n";
    fs::write(&attribution, first).unwrap();
    dispatch(&mut app, Action::BeginAdoption);
    dispatch(&mut app, Action::NextAdoptionField);
    dispatch(&mut app, Action::NextAdoptionField);
    for c in "refs/heads/main".chars() {
        dispatch(&mut app, Action::AppendAdoptionCharacter(c));
    }
    dispatch(&mut app, Action::PreviewAdoption);
    assert!(matches!(
        app.pending_adoption(),
        Some(AdoptionPrompt::Preview(_))
    ));
    fs::write(
        &attribution,
        first.replace(
            "1111111111111111111111111111111111111111",
            "2222222222222222222222222222222222222222",
        ),
    )
    .unwrap();
    app.note_detail_max_scroll(Some(0));
    dispatch(&mut app, Action::ConfirmAdoption);
    assert!(
        matches!(app.pending_adoption(), Some(AdoptionPrompt::Failed(message)) if message.contains("evidence changed"))
    );
    assert_eq!(count(&temp), 0);
}

#[test]
fn reregistering_a_replaced_checkout_refuses_without_degrading_metadata() {
    let (temp, mut app) = fixture();
    fill(&mut app);
    app.note_detail_max_scroll(Some(0));
    dispatch(&mut app, Action::ConfirmAdoption);
    dispatch(&mut app, Action::DismissAdoption);
    fs::rename(temp.path().join("source"), temp.path().join("old-source")).unwrap();
    git(
        temp.path(),
        &["clone", "--no-hardlinks", "old-source", "source"],
    );
    let preview = app.preview_source(&temp.path().join("source")).unwrap();
    assert!(matches!(
        app.confirm_source(preview),
        Err(skilled::Error::SourceHasOriginBaselines)
    ));
    assert!(app.metadata_failure().is_none());
    assert_eq!(count(&temp), 1);
}

#[test]
fn a_bare_hint_cannot_override_a_known_subdirectory_for_the_same_repository() {
    let (temp, mut app) = fixture();
    fs::write(
        temp.path().join("source/ATTRIBUTION.md"),
        "| Skill | Source | License |\n| `demo` | https://github.com/example/upstream | MIT |\n",
    )
    .unwrap();
    fs::write(temp.path().join("source/skills/demo/ATTRIBUTION.md"), "- Source snapshot: https://github.com/example/upstream/tree/1111111111111111111111111111111111111111/known/path\n").unwrap();
    dispatch(&mut app, Action::BeginAdoption);
    for field in [
        "https://github.com/example/upstream",
        "unrelated/path",
        "refs/heads/main",
    ] {
        for c in field.chars() {
            dispatch(&mut app, Action::AppendAdoptionCharacter(c));
        }
        dispatch(&mut app, Action::NextAdoptionField);
    }
    dispatch(&mut app, Action::PreviewAdoption);
    assert!(
        matches!(app.pending_adoption(), Some(AdoptionPrompt::Editing(draft)) if draft.error.is_some())
    );
    assert_eq!(count(&temp), 0);
}

#[test]
fn a_single_known_origin_path_cannot_be_overridden() {
    let (temp, mut app) = fixture();
    fs::write(temp.path().join("source/skills/demo/ATTRIBUTION.md"), "- Source snapshot: https://github.com/example/upstream/tree/1111111111111111111111111111111111111111/known/path\n").unwrap();
    dispatch(&mut app, Action::BeginAdoption);
    dispatch(&mut app, Action::NextAdoptionField);
    for _ in "known/path".chars() {
        dispatch(&mut app, Action::DeleteAdoptionCharacter);
    }
    for c in "unrelated/path".chars() {
        dispatch(&mut app, Action::AppendAdoptionCharacter(c));
    }
    dispatch(&mut app, Action::NextAdoptionField);
    for c in "refs/heads/main".chars() {
        dispatch(&mut app, Action::AppendAdoptionCharacter(c));
    }
    dispatch(&mut app, Action::PreviewAdoption);
    assert!(
        matches!(app.pending_adoption(), Some(AdoptionPrompt::Editing(draft)) if draft.error.is_some())
    );
    assert_eq!(count(&temp), 0);
}
