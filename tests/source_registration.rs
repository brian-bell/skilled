use std::{fs, path::Path, process::Command};

use skilled::{Action, AgentKind, AppEnvironment, SetupStep, SkilledApp, View};

#[test]
fn a_confirmed_source_persists_without_writing_agent_installation_roots() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let home = temporary.path().join("home");
    let data = temporary.path().join("data");
    let repository = temporary.path().join("source");
    fs::create_dir_all(repository.join("skills/portable")).expect("create common catalog");
    fs::write(
        repository.join("skills/portable/SKILL.md"),
        "---\nname: portable\ndescription: fixture\n---\n# Portable\n",
    )
    .expect("write skill");
    initialize_repository(&repository);
    let environment = AppEnvironment::new(&home, &data, "");
    let agent_roots = [
        home.join(".claude/skills"),
        home.join(".agents/skills"),
        home.join(".config/opencode/skills"),
    ];
    for root in &agent_roots {
        fs::create_dir_all(root).expect("create agent root sentinel fixture");
        fs::write(root.join("sentinel"), "unchanged").expect("write agent root sentinel");
    }

    let mut app = SkilledApp::open(environment.clone()).expect("open application");
    let preview = app
        .preview_source(&repository)
        .expect("preview local source");
    assert_eq!(preview.catalogs().len(), 1);
    app.confirm_source(preview).expect("confirm local source");
    drop(app);

    for root in agent_roots {
        assert_eq!(
            fs::read_to_string(root.join("sentinel")).expect("read agent root sentinel"),
            "unchanged"
        );
        assert_eq!(
            fs::read_dir(&root).expect("read agent root").count(),
            1,
            "registration changed {}",
            root.display()
        );
    }

    let reopened = SkilledApp::open(environment).expect("reopen application");
    assert_eq!(reopened.sources().len(), 1);
    assert_eq!(
        reopened.sources()[0].git_top_level(),
        repository.canonicalize().unwrap()
    );
    assert_eq!(reopened.sources()[0].catalogs().len(), 1);
    assert!(reopened.sources()[0].last_scan_at() > 0);
    assert_eq!(
        reopened.sources()[0].catalogs()[0]
            .relative_path()
            .to_string_lossy(),
        "skills"
    );
}

#[test]
fn setup_can_confirm_and_correct_a_detected_catalog_before_registration() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let repository = temporary.path().join("source");
    fs::create_dir_all(repository.join("skills/portable")).expect("create common catalog");
    fs::write(
        repository.join("skills/portable/SKILL.md"),
        "---\nname: portable\ndescription: fixture\n---\n# Portable\n",
    )
    .expect("write skill");
    initialize_repository(&repository);
    let environment = AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    );
    let mut app = SkilledApp::open(environment.clone()).expect("open application");
    for _ in 0..3 {
        dispatch(&mut app, Action::Continue);
    }
    assert_eq!(app.view(), View::Setup(SetupStep::DiscoverSources));

    app.update(Action::BeginAddSource);
    for character in repository.to_string_lossy().chars() {
        app.update(Action::AppendSourcePath(character));
    }
    dispatch(&mut app, Action::SubmitSourcePath);
    assert_eq!(app.view(), View::Setup(SetupStep::ConfirmCatalogs));

    app.update(Action::CancelSourceFlow);
    assert_eq!(app.view(), View::Setup(SetupStep::DiscoverSources));
    app.update(Action::BeginAddSource);
    for character in repository.to_string_lossy().chars() {
        app.update(Action::AppendSourcePath(character));
    }
    dispatch(&mut app, Action::SubmitSourcePath);

    app.update(Action::ToggleCatalogClassification);
    app.update(Action::ToggleCatalogCompatibility(AgentKind::OpenCode));
    dispatch(&mut app, Action::Continue);
    assert_eq!(app.view(), View::Setup(SetupStep::ScanInstallations));
    drop(app);

    let reopened = SkilledApp::open(environment).expect("reopen application");
    let catalog = &reopened.sources()[0].catalogs()[0];
    assert_eq!(
        catalog.classification(),
        skilled::source::CatalogClassification::AgentSpecific
    );
    assert!(!catalog.compatibility().opencode());
}

#[test]
fn source_paths_expand_a_leading_home_component_before_git_inspection() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let home = temporary.path().join("home");
    let repository = home.join("source");
    fs::create_dir_all(repository.join("skills/portable")).expect("create common catalog");
    fs::write(
        repository.join("skills/portable/SKILL.md"),
        "---\nname: portable\ndescription: fixture\n---\n# Portable\n",
    )
    .expect("write skill");
    initialize_repository(&repository);
    let app = SkilledApp::open(AppEnvironment::new(
        &home,
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");

    let preview = app
        .preview_source(Path::new("~/source"))
        .expect("expand home-relative source path");

    assert_eq!(
        preview.inspected().git_top_level(),
        repository.canonicalize().unwrap()
    );
}

#[test]
fn a_one_skill_repository_round_trips_as_a_root_catalog() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let repository = temporary.path().join("only-skill");
    fs::create_dir(&repository).expect("create one-skill repository");
    fs::write(
        repository.join("SKILL.md"),
        "---\nname: only-skill\ndescription: fixture\n---\n# Only skill\n",
    )
    .expect("write skill");
    initialize_repository(&repository);
    let environment = AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    );
    let mut app = SkilledApp::open(environment.clone()).expect("open application");
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("confirm source");
    drop(app);

    let reopened = SkilledApp::open(environment).expect("reopen application");
    let catalog = &reopened.sources()[0].catalogs()[0];
    assert_eq!(catalog.relative_path(), Path::new("."));
    assert_eq!(catalog.candidates()[0].relative_path(), Path::new("."));
    assert!(catalog.candidates()[0].validation().is_valid());
}

fn initialize_repository(repository: &Path) {
    git(repository, &["init", "-b", "main"]);
    git(repository, &["config", "user.name", "Skilled Test"]);
    git(
        repository,
        &["config", "user.email", "skilled@example.test"],
    );
    git(repository, &["add", "."]);
    git(repository, &["commit", "-m", "fixture"]);
}

fn git(repository: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .expect("run Git fixture command");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn dispatch(app: &mut SkilledApp, action: Action) {
    let update = app.update(action);
    app.perform_effects(update.effects())
        .expect("perform effects");
}
