use std::{fs, path::Path, process::Command};

use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use skilled::{Action, AgentKind, AppEnvironment, SkilledApp};

#[test]
fn first_run_welcome_at_minimum_supported_size() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");

    insta::assert_snapshot!(render(&app, 80, 24));
}

#[test]
fn first_run_welcome_at_wide_size() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");

    insta::assert_snapshot!(render(&app, 120, 40));
}

#[test]
fn contextual_help_at_minimum_supported_size() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    app.update(Action::Continue);
    app.update(Action::OpenHelp);

    insta::assert_snapshot!(render(&app, 80, 24));
}

#[test]
fn contextual_help_at_wide_size() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    for _ in 0..7 {
        dispatch(&mut app, Action::Continue);
    }
    app.update(Action::OpenSources);
    app.update(Action::OpenHelp);

    insta::assert_snapshot!(render(&app, 120, 40));
}

#[test]
fn inventory_empty_state_at_minimum_supported_size() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    for _ in 0..7 {
        dispatch(&mut app, Action::Continue);
    }

    insta::assert_snapshot!(render(&app, 80, 24));
}

#[test]
fn inventory_empty_state_at_wide_size() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    for _ in 0..7 {
        dispatch(&mut app, Action::Continue);
    }

    insta::assert_snapshot!(render(&app, 120, 40));
}

#[test]
fn undersized_terminal_shows_a_recoverable_notice() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");

    insta::assert_snapshot!(render(&app, 60, 14));
}

#[test]
fn detected_agents_and_selection_fit_at_minimum_supported_size() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    app.update(Action::Continue);
    app.update(Action::MoveSelection(1));
    app.update(Action::ToggleSelection);

    insta::assert_snapshot!(render(&app, 80, 24));
}

#[test]
fn remaining_setup_steps_fit_at_minimum_supported_size() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    app.update(Action::Continue);
    app.update(Action::Continue);

    insta::assert_snapshot!(
        "choose_scan_roots_at_minimum_supported_size",
        render(&app, 80, 24)
    );
    app.update(Action::Continue);
    insta::assert_snapshot!(
        "discover_sources_at_minimum_supported_size",
        render(&app, 80, 24)
    );
    app.update(Action::Continue);
    insta::assert_snapshot!(
        "confirm_catalogs_at_minimum_supported_size",
        render(&app, 80, 24)
    );
    app.update(Action::Continue);
    insta::assert_snapshot!(
        "scan_installations_at_minimum_supported_size",
        render(&app, 80, 24)
    );
    app.update(Action::Continue);
    insta::assert_snapshot!(
        "setup_summary_at_minimum_supported_size",
        render(&app, 80, 24)
    );
}

#[test]
fn settings_dialog_at_compact_and_wide_sizes() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    for _ in 0..7 {
        dispatch(&mut app, Action::Continue);
    }
    app.update(Action::OpenSettings);

    insta::assert_snapshot!(
        "settings_dialog_at_minimum_supported_size",
        render(&app, 80, 24)
    );
    insta::assert_snapshot!("settings_dialog_at_wide_size", render(&app, 120, 40));
}

#[test]
fn add_source_path_entry_at_minimum_supported_size() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    for _ in 0..7 {
        let update = app.update(Action::Continue);
        app.perform_effects(update.effects()).unwrap();
    }
    app.update(Action::OpenSources);
    app.update(Action::BeginAddSource);
    for character in "/Users/example/dev/skills".chars() {
        app.update(Action::AppendSourcePath(character));
    }

    insta::assert_snapshot!(render(&app, 80, 24));
}

#[test]
fn catalog_confirmation_at_minimum_supported_size() {
    let temporary = tempfile::Builder::new()
        .prefix("skilled-")
        .tempdir_in("/tmp")
        .expect("temporary application directory");
    let repository = temporary.path().join("source");
    create_source_fixture(&repository);
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    for _ in 0..7 {
        dispatch(&mut app, Action::Continue);
    }
    app.update(Action::OpenSources);
    app.update(Action::BeginAddSource);
    for character in repository.join("skills/portable").to_string_lossy().chars() {
        app.update(Action::AppendSourcePath(character));
    }
    dispatch(&mut app, Action::SubmitSourcePath);

    let preview = app.pending_source().expect("pending source preview");
    let rendered = render(&app, 80, 24)
        .replace(
            temporary
                .path()
                .canonicalize()
                .expect("canonical temporary directory")
                .to_string_lossy()
                .as_ref(),
            "[TEMP]",
        )
        .replace(&preview.inspected().head()[..8], "[HEAD]");
    insta::assert_snapshot!(rendered);
}

#[test]
fn sources_browse_valid_and_invalid_immediate_variants_without_nested_examples() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let repository = temporary.path().join("source");
    create_source_fixture(&repository);
    fs::create_dir_all(repository.join("skills/broken")).expect("create invalid candidate");
    fs::write(repository.join("skills/broken/skill.md"), "wrong filename")
        .expect("write invalid candidate");
    fs::create_dir_all(repository.join("skills/portable/references/example"))
        .expect("create nested example");
    fs::write(
        repository.join("skills/portable/references/example/SKILL.md"),
        "---\nname: example\ndescription: Nested example\n---\n# Example\n",
    )
    .expect("write nested example");
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "add edge cases"]);
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("confirm source");
    for _ in 0..7 {
        dispatch(&mut app, Action::Continue);
    }
    app.update(Action::OpenSources);

    let screen = render(&app, 120, 40);

    assert!(screen.contains("✓ portable"));
    assert!(screen.contains("× broken"));
    assert!(!screen.contains("Nested example"));
}

#[test]
fn sources_show_the_persisted_catalog_classification_and_compatibility() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let repository = temporary.path().join("source");
    create_source_fixture(&repository);
    let environment = AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    );
    let mut app = SkilledApp::open(environment.clone()).expect("open application");
    for _ in 0..3 {
        dispatch(&mut app, Action::Continue);
    }
    app.update(Action::BeginAddSource);
    for character in repository.to_string_lossy().chars() {
        app.update(Action::AppendSourcePath(character));
    }
    dispatch(&mut app, Action::SubmitSourcePath);
    app.update(Action::ToggleCatalogClassification);
    app.update(Action::ToggleCatalogCompatibility(AgentKind::OpenCode));
    dispatch(&mut app, Action::ConfirmPendingSource);
    dispatch(&mut app, Action::Continue);
    dispatch(&mut app, Action::Continue);
    drop(app);

    let mut reopened = SkilledApp::open(environment).expect("reopen application");
    reopened.update(Action::OpenSources);
    let screen = render(&reopened, 120, 40);

    assert!(screen.contains("Agent-specific"));
    assert!(screen.contains("Claude: yes · Codex: yes · OpenCode: no"));
}

#[test]
fn sources_escape_control_characters_from_skill_metadata() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let repository = temporary.path().join("source");
    fs::create_dir_all(repository.join("skills/portable")).expect("create skill directory");
    fs::write(
        repository.join("skills/portable/SKILL.md"),
        "---\nname: portable\ndescription: \"fixture\\u001b]8;;https://example.test\"\n---\n# Fixture\n",
    )
    .expect("write control-character fixture");
    git(&repository, &["init", "-b", "main"]);
    git(&repository, &["config", "user.name", "Skilled Test"]);
    git(
        &repository,
        &["config", "user.email", "skilled@example.test"],
    );
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "fixture"]);
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("confirm source");
    for _ in 0..7 {
        dispatch(&mut app, Action::Continue);
    }
    app.update(Action::OpenSources);

    let screen = render(&app, 120, 40);

    assert!(!screen.contains('\u{1b}'));
    assert!(screen.contains("fixture\\u{1b}]8;;https://example.test"));
}

#[test]
fn sources_keeps_a_variant_selection_and_details_visible_beyond_the_first_viewport() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let repository = temporary.path().join("source");
    create_source_fixture(&repository);
    for index in 0..30 {
        let name = format!("skill-{index:02}-with-a-long-directory-name");
        let description = if index == 24 {
            format!("Fixture {index} {}", "detail".repeat(150))
        } else {
            format!("Fixture {index}")
        };
        fs::create_dir_all(repository.join("skills").join(&name)).expect("create skill");
        fs::write(
            repository.join("skills").join(&name).join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n# Fixture\n"),
        )
        .expect("write skill");
    }
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "add many skills"]);
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("confirm source");
    for _ in 0..7 {
        dispatch(&mut app, Action::Continue);
    }
    app.update(Action::OpenSources);
    app.update(Action::ToggleSourcesPane);
    for _ in 0..25 {
        app.update(Action::MoveSourcesSelection(1));
    }
    let expected = app.sources()[0].catalogs()[0].candidates()[25]
        .directory_name()
        .to_owned();
    let expected_description = "Fixture 24";

    let screen = render(&app, 80, 24);

    assert_eq!(app.focused_variant(), 25);
    assert!(screen.contains(&format!("▌ ✓ {expected}")));
    assert!(screen.contains("(skills)"), "{screen}");
    assert!(screen.contains(expected_description), "{screen}");
}

#[test]
fn catalog_confirmation_keeps_the_focused_root_visible() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let repository = temporary.path().join("source");
    for index in 0..8 {
        let name = format!("skill-{index}");
        let directory = repository
            .join("catalogs")
            .join(format!("set-{index}"))
            .join("claude-code/skills")
            .join(&name);
        fs::create_dir_all(&directory).expect("create catalog fixture");
        fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Fixture\n---\n# Fixture\n"),
        )
        .expect("write skill");
    }
    git(&repository, &["init", "-b", "main"]);
    git(&repository, &["config", "user.name", "Skilled Test"]);
    git(
        &repository,
        &["config", "user.email", "skilled@example.test"],
    );
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "fixture"]);
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    for _ in 0..7 {
        dispatch(&mut app, Action::Continue);
    }
    app.update(Action::OpenSources);
    app.update(Action::BeginAddSource);
    for character in repository.to_string_lossy().chars() {
        app.update(Action::AppendSourcePath(character));
    }
    dispatch(&mut app, Action::SubmitSourcePath);
    for _ in 0..7 {
        app.update(Action::MoveCatalogSelection(1));
    }

    let screen = render(&app, 80, 24);

    assert_eq!(app.focused_catalog(), 7);
    assert!(screen.contains("set-7"));
    assert!(!screen.contains("set-0"));
}

#[test]
fn wrapped_catalog_confirmation_keeps_focus_error_and_actions_visible() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let repository = temporary
        .path()
        .join("a-deliberately-long-source-repository-directory-name");
    for index in 0..2 {
        let name = format!("skill-{index}");
        let directory = repository
            .join("catalogs")
            .join(format!(
                "set-{index}-with-a-deliberately-long-catalog-root-name"
            ))
            .join("claude-code/skills")
            .join(&name);
        fs::create_dir_all(&directory).expect("create catalog fixture");
        fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Fixture\n---\n# Fixture\n"),
        )
        .expect("write skill");
    }
    git(&repository, &["init", "-b", "main"]);
    git(&repository, &["config", "user.name", "Skilled Test"]);
    git(
        &repository,
        &["config", "user.email", "skilled@example.test"],
    );
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "fixture"]);
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    for _ in 0..7 {
        dispatch(&mut app, Action::Continue);
    }
    app.update(Action::OpenSources);
    app.update(Action::BeginAddSource);
    for character in repository.to_string_lossy().chars() {
        app.update(Action::AppendSourcePath(character));
    }
    dispatch(&mut app, Action::SubmitSourcePath);
    app.update(Action::ToggleCatalogIncluded);
    app.update(Action::MoveCatalogSelection(1));
    app.update(Action::ToggleCatalogIncluded);
    app.update(Action::ConfirmPendingSource);

    let screen = render(&app, 80, 24);

    assert_eq!(app.focused_catalog(), 1);
    assert!(screen.contains("> [ ]"), "{screen}");
    assert!(screen.contains("catalogs/set-1"), "{screen}");
    assert!(
        screen.contains("Select at least one catalog root to register."),
        "{screen}"
    );
    assert!(screen.contains("Enter registers metadata only"), "{screen}");
}

fn render(app: &SkilledApp, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("create test terminal");
    terminal
        .draw(|frame| skilled::tui::render(frame, app))
        .expect("render frame");
    buffer_text(terminal.backend().buffer())
}

fn buffer_text(buffer: &Buffer) -> String {
    let area = buffer.area;
    (area.y..area.y + area.height)
        .map(|y| {
            let mut line = String::new();
            for x in area.x..area.x + area.width {
                line.push_str(buffer[(x, y)].symbol());
            }
            line.trim_end().to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn create_source_fixture(repository: &Path) {
    fs::create_dir_all(repository.join("skills/portable")).expect("create source fixture");
    fs::write(
        repository.join("skills/portable/SKILL.md"),
        "---\nname: portable\ndescription: Portable fixture\n---\n# Portable\n",
    )
    .expect("write source fixture");
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
    assert!(output.status.success());
}

fn dispatch(app: &mut SkilledApp, action: Action) {
    let update = app.update(action);
    app.perform_effects(update.effects()).unwrap();
}
