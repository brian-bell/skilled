use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use skilled::{Action, AgentKind, AppEnvironment, SkilledApp, tui::RenderFeedback};

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
    // Step six is where the roots are read, so its effect has to run for the
    // step to report anything.
    dispatch(&mut app, Action::Continue);
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
    insta::assert_snapshot!("add_source_path_entry_at_wide_size", render(&app, 120, 40));
}

#[test]
fn add_source_wrapped_inspection_error_at_minimum_supported_size() {
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
    app.update(Action::BeginAddSource);
    // A fixed, never-real path rather than one under `temporary`: it only
    // needs to be long and absent, and a literal keeps the wrap position
    // independent of the host's temporary-directory path length.
    let missing = PathBuf::from(
        "/a-deliberately-long-missing-repository-directory/and-an-equally-long-nested-path",
    );
    for character in missing.to_string_lossy().chars() {
        app.update(Action::AppendSourcePath(character));
    }
    dispatch(&mut app, Action::SubmitSourcePath);

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
    let short_head = &preview.inspected().head()[..8];
    let rendered = normalize_snapshot_field(render(&app, 80, 24), "Repository: ", "[TEMP]/source")
        .replace(short_head, &padded_placeholder(short_head, "[HEAD]"));
    insta::assert_snapshot!(rendered);

    let rendered = normalize_snapshot_field(render(&app, 120, 40), "Repository: ", "[TEMP]/source")
        .replace(short_head, &padded_placeholder(short_head, "[HEAD]"));
    insta::assert_snapshot!("catalog_confirmation_at_wide_size", rendered);
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

    assert!(screen.contains("✓ valid portable"));
    assert!(screen.contains("× invalid broken"));
    assert!(!screen.contains("Nested example"));
}

#[test]
fn sources_show_the_persisted_catalog_classification_and_registration() {
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
    // The claim names the agents the catalog is registered for and stops; the
    // agent switched off during setup is one of the ones it does not name.
    assert!(
        screen.contains("Registered for: Claude Code + Codex"),
        "{screen}"
    );
    assert!(!screen.contains("OpenCode"), "{screen}");
}

#[test]
fn responsive_sources_workspace_at_wide_and_compact_sizes() {
    let temporary = tempfile::tempdir_in("/tmp").expect("temporary application directory");
    let repository = temporary.path().join("source");
    create_source_fixture(&repository);
    fs::create_dir_all(repository.join("skills/broken")).expect("create invalid candidate");
    fs::write(repository.join("skills/broken/skill.md"), "wrong filename")
        .expect("write invalid candidate");
    // A second catalog, classified and claimed differently from the first, so
    // the grouped list shows two labels that do not read alike.
    let agent_specific = repository.join("experimental/claude-code/skills/experimental");
    fs::create_dir_all(&agent_specific).expect("create agent-specific catalog");
    fs::write(
        agent_specific.join("SKILL.md"),
        "---\nname: experimental\ndescription: Experimental fixture\n---\n# Experimental\n",
    )
    .expect("write agent-specific candidate");
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "add invalid candidate"]);
    git(
        &repository,
        &["remote", "add", "origin", "https://example.test/source.git"],
    );
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
    app.update(Action::MoveSourcesPane(1));
    app.update(Action::MoveSourcesSelection(1));
    app.update(Action::MoveSourcesPane(-1));

    insta::assert_snapshot!(
        "sources_populated_at_wide_size",
        normalize_sources_screen(&app, &temporary, render(&app, 120, 40))
    );
    insta::assert_snapshot!(
        "sources_repositories_at_minimum_supported_size",
        normalize_sources_screen(&app, &temporary, render(&app, 80, 24))
    );

    app.update(Action::AdvanceSourcesPane);
    insta::assert_snapshot!(
        "sources_variants_at_minimum_supported_size",
        normalize_sources_screen(&app, &temporary, render(&app, 80, 24))
    );

    app.update(Action::AdvanceSourcesPane);
    insta::assert_snapshot!(
        "sources_details_at_minimum_supported_size",
        normalize_sources_screen(&app, &temporary, render(&app, 80, 24))
    );
}

/// A catalog path deep enough to outrun the region it is stated in. Every
/// path field is one line cut in its middle, and the classification the
/// catalog path is stated with is never cut to make room: where the two do not
/// share a line, it takes the line below.
#[test]
fn sources_details_with_a_long_catalog_path_at_wide_and_compact_sizes() {
    let temporary = tempfile::tempdir_in("/tmp").expect("temporary application directory");
    let repository = temporary.path().join("source");
    let variant = repository.join("deeply/nested/experimental/claude-code/skills/experimental");
    fs::create_dir_all(&variant).expect("create nested catalog");
    fs::write(
        variant.join("SKILL.md"),
        "---\nname: experimental\ndescription: Experimental fixture\n---\n# Experimental\n",
    )
    .expect("write nested candidate");
    git(&repository, &["init", "-b", "main"]);
    git(&repository, &["config", "user.name", "Skilled Test"]);
    git(
        &repository,
        &["config", "user.email", "skilled@example.test"],
    );
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "fixture"]);
    git(
        &repository,
        &[
            "remote",
            "add",
            "origin",
            "https://example.test/an-organisation-with-a-long-name/source.git",
        ],
    );
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

    insta::assert_snapshot!(
        "sources_long_catalog_path_at_wide_size",
        normalize_sources_screen(&app, &temporary, render(&app, 120, 40))
    );

    app.update(Action::AdvanceSourcesPane);
    app.update(Action::AdvanceSourcesPane);
    insta::assert_snapshot!(
        "sources_long_catalog_path_at_minimum_supported_size",
        normalize_sources_screen(&app, &temporary, render(&app, 80, 24))
    );
}

/// Detail that outgrows the Sources region says how much it left out, the way
/// the Inventory region does. A region that simply ends mid-sentence reads as
/// though the description it was stating had ended there.
#[test]
fn sources_detail_too_tall_for_the_region_reports_what_it_dropped() {
    let temporary = tempfile::tempdir_in("/tmp").expect("temporary application directory");
    let repository = temporary.path().join("source");
    let variant = repository.join("skills/verbose");
    fs::create_dir_all(&variant).expect("create catalog fixture");
    fs::write(
        variant.join("SKILL.md"),
        "---\nname: verbose\ndescription: A description long enough to outgrow the detail \
         region at twenty-four rows, so the region has to say what it could not \
         show rather than ending in the middle of this sentence.\n---\n# Verbose\n",
    )
    .expect("write catalog fixture");
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

    let screen = normalize_sources_screen(&app, &temporary, render(&app, 120, 24));
    assert!(screen.contains("more line"), "{screen}");
    insta::assert_snapshot!("sources_detail_too_tall_at_wide_size", screen);
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
    assert!(screen.contains("fixture\\u{1b}]8;;https"), "{screen}");
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
    app.update(Action::MoveSourcesPane(1));
    for _ in 0..25 {
        app.update(Action::MoveSourcesSelection(1));
    }
    let expected = app.sources()[0].catalogs()[0].candidates()[25]
        .directory_name()
        .to_owned();
    let expected_description = "Fixture 24";

    let variants = render(&app, 80, 24);

    assert_eq!(app.focused_variant(), 25);
    assert!(variants.contains(&format!("▌ ✓ valid {expected}")));
    // The catalog is named once, by the group label above its variants, so a
    // row deep in the list does not repeat the path it sits in. Which means
    // the label has to stay on screen once the list scrolls past it: pinned to
    // the first row of the pane, under the header and its rule.
    assert!(!variants.contains("(skills/"), "{variants}");
    assert!(
        variants.lines().nth(4).is_some_and(|line| {
            line.starts_with("skills  ") && line.trim_end().ends_with("Common · all agents")
        }),
        "{variants}"
    );

    app.update(Action::AdvanceSourcesPane);
    let details = render(&app, 80, 24);
    assert!(details.contains(expected_description), "{details}");
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
    assert!(screen.contains("▌ Excluded"), "{screen}");
    assert!(screen.contains("catalogs/set-1"), "{screen}");
    assert!(
        screen.contains("Select at least one catalog root to register."),
        "{screen}"
    );
    assert!(
        screen.contains("Registration records metadata only"),
        "{screen}"
    );
    assert!(screen.contains("Esc Cancel   Enter Register"), "{screen}");
}

#[test]
fn setup_catalog_confirmation_reserves_space_for_wrapped_focused_content() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let repository = temporary
        .path()
        .join("a-deliberately-long-source-repository-directory-name");
    for index in 0..6 {
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
    for _ in 0..3 {
        dispatch(&mut app, Action::Continue);
    }
    app.update(Action::BeginAddSource);
    for character in repository.to_string_lossy().chars() {
        app.update(Action::AppendSourcePath(character));
    }
    dispatch(&mut app, Action::SubmitSourcePath);
    for _ in 0..5 {
        app.update(Action::MoveCatalogSelection(1));
    }

    let preview = app.pending_source().expect("pending source preview");
    let short_head = &preview.inspected().head()[..8];
    let rendered =
        normalize_snapshot_field(render(&app, 80, 24), "Repository: ", "[TEMP]/long-source")
            .replace(short_head, &padded_placeholder(short_head, "[HEAD]"));

    assert_eq!(app.focused_catalog(), 5);
    assert!(rendered.contains("▌ Included"), "{rendered}");
    assert!(rendered.contains("catalogs/set-5"), "{rendered}");
    assert!(
        rendered.contains("Repository: [TEMP]/long-source"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Registration records metadata only"),
        "{rendered}"
    );
    insta::assert_snapshot!(rendered);
}

#[cfg(unix)]
mod installed {
    use super::*;
    use skilled::InventoryPane;

    #[test]
    fn inventory_populated_at_minimum_supported_size() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let app = inventory_app(&temporary);

        insta::assert_snapshot!(normalize_inventory(&temporary, render(&app, 80, 24)));
    }

    #[test]
    fn inventory_populated_at_wide_size() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let app = inventory_app(&temporary);

        insta::assert_snapshot!(normalize_inventory(&temporary, render(&app, 120, 40)));
    }

    /// The narrowest wide viewport, where the primary region is only sixty
    /// columns and the Source column is dropped rather than truncated into an
    /// ellipsis that distinguishes nothing.
    #[test]
    fn inventory_at_the_wide_breakpoint() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let app = inventory_app(&temporary);

        // A 100×26 workspace inside the window frame: the breakpoint is the
        // workspace's, two columns and rows inside the terminal's.
        let screen = normalize_inventory(&temporary, render(&app, 102, 28));

        // Scoped to the table's heading row: the detail region beside it has a
        // SOURCE section of its own, which is exactly where the dropped column
        // still names the source.
        assert!(!heading_row(&screen).contains("SOURCE"), "{screen}");
        assert!(screen.contains("unman"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    /// On a very wide terminal the identity columns stop growing, so a short
    /// label is not stranded in the middle of a very wide field — that grid
    /// grows these columns without bound in the prototype. The agent columns
    /// spend part of the freed width on the health labels the prototype's
    /// `.agent-state` cells carry, and what remains falls to the right of
    /// Health.
    #[test]
    fn inventory_capped_columns_at_a_very_wide_size() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let app = inventory_app(&temporary);

        let screen = normalize_inventory(&temporary, render(&app, 180, 40));

        let headings = heading_row(&screen);
        assert_eq!(column_of(headings, "SOURCE"), 38, "{headings:?}");
        assert_eq!(column_of(headings, "CLAUDE"), 62, "{headings:?}");
        insta::assert_snapshot!(screen);
    }

    /// The table side of the heading row, read past the window frame's own
    /// column and cut at the detail region's separator, so neither the frame
    /// nor anything the detail pane happens to render can answer for the
    /// table's columns.
    fn heading_row(screen: &str) -> &str {
        let row = screen
            .lines()
            .find(|line| line.contains("HEALTH"))
            .unwrap_or_else(|| panic!("no heading row in\n{screen}"));
        let row = row.strip_prefix('▕').unwrap_or(row);
        row.split('│').next().unwrap_or(row)
    }

    /// The content column a heading starts at, counted in characters because
    /// the chrome around the table is single width.
    fn column_of(line: &str, needle: &str) -> usize {
        let byte_index = line
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} not found in {line:?}"));
        line[..byte_index].chars().count()
    }

    /// A root Skilled could not read reports the reason, because it
    /// contributes nothing else.
    #[test]
    fn inventory_with_an_unreadable_root_at_minimum_supported_size() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let home = temporary.path().join("home");
        fs::create_dir_all(home.join(".claude")).expect("create Claude Code parent");
        fs::write(home.join(".claude/skills"), "not a directory")
            .expect("write a file where the root belongs");
        let mut app = SkilledApp::open(AppEnvironment::new(
            &home,
            temporary.path().join("data"),
            "",
        ))
        .expect("open application");
        for _ in 0..7 {
            dispatch(&mut app, Action::Continue);
        }

        insta::assert_snapshot!(normalize_inventory(&temporary, render(&app, 80, 24)));
    }

    /// Setup step six with something to report, and the summary that counts it.
    #[test]
    fn populated_scan_installations_and_summary_at_minimum_supported_size() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let mut app = inventory_app(&temporary);
        app.update(Action::OpenSettings);
        dispatch(&mut app, Action::RerunSetup);
        for _ in 0..5 {
            dispatch(&mut app, Action::Continue);
        }

        let step_six = normalize_inventory(&temporary, render(&app, 80, 24));
        assert!(step_six.contains("STEP 6 / 7"), "{step_six}");
        insta::assert_snapshot!("populated_scan_installations", step_six);

        dispatch(&mut app, Action::Continue);
        let summary = normalize_inventory(&temporary, render(&app, 80, 24));
        assert!(summary.contains("STEP 7 / 7"), "{summary}");
        insta::assert_snapshot!("populated_summary", summary);
    }

    #[test]
    fn inventory_broken_installation_detail_at_wide_size() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let mut app = inventory_app(&temporary);
        app.update(Action::MoveInventorySelection(1));

        assert_eq!(
            app.selected_installation().map(|row| row.name().to_owned()),
            Some("broken".to_owned())
        );
        insta::assert_snapshot!(normalize_inventory(&temporary, render(&app, 120, 40)));
    }

    #[test]
    fn inventory_detail_drill_in_at_minimum_supported_size() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let mut app = inventory_app(&temporary);
        app.update(Action::MoveInventorySelection(2));
        app.update(Action::AdvanceInventoryPane);

        assert_eq!(app.inventory_pane(), InventoryPane::Details);
        insta::assert_snapshot!(normalize_inventory(&temporary, render(&app, 80, 24)));
    }

    /// Detail that outgrows the region says how much it left out, because a
    /// pane that simply ends mid-section reads as though there were no more —
    /// and, holding the keyboard, says how the rest is reached.
    #[test]
    fn inventory_detail_too_tall_for_the_region_reports_what_it_dropped() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let mut app = inventory_app(&temporary);
        app.update(Action::AdvanceInventoryPane);

        let screen = normalize_inventory(&temporary, render(&app, 80, 24));
        assert!(
            screen.contains("! 11 more lines below — j/k to scroll"),
            "{screen}"
        );
        insta::assert_snapshot!(screen);

        // Beside the table the region is only thirty-seven cells wide, and the
        // keys belong to the table: the notice names the focus that reaches
        // the window before the keys that move it, and that form still fits.
        // A 100×26 workspace inside the window frame.
        app.update(Action::MoveInventoryPane(1));
        let beside = normalize_inventory(&temporary, render(&app, 102, 28));
        assert!(
            beside.contains("! 9 more lines below — Tab, then j/k"),
            "{beside}"
        );

        // With the query box holding every printable key, no keystroke reaches
        // those rows and the notice falls back to advising a bigger terminal —
        // a phrase too long for this region, so it gives up its words rather
        // than wrapping off the bottom: the one line whose job is to report a
        // cut must not itself be cut.
        app.update(Action::BeginInventoryFilter);
        assert!(app.inventory_filter_active());
        let narrow = normalize_inventory(&temporary, render(&app, 102, 28));
        assert!(narrow.contains("! 9 more lines"), "{narrow}");
        assert!(!narrow.contains("widen or lengthen"), "{narrow}");
        assert!(!narrow.contains("more lines below"), "{narrow}");
    }

    /// The rows a cramped region cannot show are reachable rather than merely
    /// counted. Scrolled as far as the frame reported, the window ends on the
    /// last row of the content and says how much of it is now above.
    #[test]
    fn inventory_detail_scrolled_to_its_end_at_minimum_supported_size() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let mut app = inventory_app(&temporary);
        app.update(Action::AdvanceInventoryPane);
        scroll_detail_to_the_end(&mut app, 80, 24);

        let screen = normalize_inventory(&temporary, render(&app, 80, 24));
        assert!(screen.contains("! 11 lines above"), "{screen}");
        assert!(!screen.contains("more lines below"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    /// Scroll the detail region the way the runner does: draw, take the
    /// frame's report back to the application, and move by what it measured.
    fn scroll_detail_to_the_end(app: &mut SkilledApp, width: u16, height: u16) {
        let extent = drawn(app, width, height)
            .1
            .detail_max_scroll()
            .expect("the frame drew the detail region");
        assert!(extent > 0, "the region should have somewhere to scroll");
        app.note_detail_max_scroll(Some(extent));
        for _ in 0..extent {
            app.update(Action::ScrollDetail(1));
        }
    }

    #[test]
    fn inventory_filter_entry_at_minimum_supported_size() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let mut app = inventory_app(&temporary);
        app.update(Action::BeginInventoryFilter);
        for character in "unman".chars() {
            app.update(Action::AppendInventoryFilter(character));
        }

        insta::assert_snapshot!(normalize_inventory(&temporary, render(&app, 80, 24)));
    }

    #[test]
    fn inventory_filter_without_matches_at_minimum_supported_size() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let mut app = inventory_app(&temporary);
        app.update(Action::BeginInventoryFilter);
        for character in "nothing".chars() {
            app.update(Action::AppendInventoryFilter(character));
        }
        app.update(Action::SubmitInventoryFilter);

        assert!(app.filtered_rows().is_empty());
        insta::assert_snapshot!(normalize_inventory(&temporary, render(&app, 80, 24)));
    }

    /// Every classification the effective resolution can reach, plus a broken
    /// installation, so the Doctor list shows its whole ordering at once.
    ///
    /// The source checkouts live inside the temporary home so every path the
    /// screen shows is home-relative and therefore stable across machines.
    fn doctor_app(temporary: &tempfile::TempDir) -> SkilledApp {
        let home = temporary.path().join("home");
        let first = home.join("alpha");
        write_skill(&first.join("skills/review"), "review");
        write_skill(&first.join("skills/shared"), "shared");
        write_skill(&first.join("claude/skills/exposed"), "exposed");
        create_repository(&first);
        let second = home.join("beta");
        write_skill(&second.join("skills/review"), "review");
        create_repository(&second);
        let third = home.join("gamma");
        write_skill(&third.join("skills/excluded"), "excluded");
        create_repository(&third);

        let mut app = SkilledApp::open(AppEnvironment::new(
            &home,
            temporary.path().join("data"),
            "",
        ))
        .expect("open application");
        for _ in 0..7 {
            dispatch(&mut app, Action::Continue);
        }
        for repository in [&first, &second] {
            let preview = app.preview_source(repository).expect("preview source");
            app.confirm_source(preview).expect("register source");
        }
        app.update(Action::OpenSources);
        app.update(Action::BeginAddSource);
        for character in third.to_string_lossy().chars() {
            app.update(Action::AppendSourcePath(character));
        }
        dispatch(&mut app, Action::SubmitSourcePath);
        app.update(Action::ToggleCatalogCompatibility(AgentKind::OpenCode));
        dispatch(&mut app, Action::ConfirmPendingSource);

        let claude = home.join(".claude/skills");
        let codex = home.join(".agents/skills");
        let opencode = home.join(".config/opencode/skills");
        for root in [&claude, &codex, &opencode] {
            fs::create_dir_all(root).expect("create agent skill root");
        }
        // One name, two directories: a conflicting duplicate for OpenCode.
        link(&first.join("skills/review"), &opencode.join("review"));
        link(&second.join("skills/review"), &claude.join("review"));
        // One directory through two roots: a benign alias.
        link(&first.join("skills/shared"), &claude.join("shared"));
        link(&first.join("skills/shared"), &codex.join("shared"));
        // A Claude Code edition OpenCode can see but Skilled cannot claim.
        link(
            &first.join("claude/skills/exposed"),
            &claude.join("exposed"),
        );
        // A common variant OpenCode can reach whose catalog excludes it.
        link(&third.join("skills/excluded"), &claude.join("excluded"));
        // And a link with nothing behind it.
        link(&home.join("gone"), &claude.join("dangling"));

        app.update(Action::OpenSources);
        dispatch(&mut app, Action::OpenDoctor);
        app
    }

    #[test]
    fn doctor_populated_at_minimum_supported_size() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let app = doctor_app(&temporary);

        let screen = normalize_inventory(&temporary, render(&app, 80, 24));

        // Issue-first: the broken installation leads, and the informational
        // alias is ranked below everything that weakens usability.
        let codes: Vec<&str> = screen
            .lines()
            .filter_map(|line| line.split_whitespace().find(|word| word.contains('.')))
            .collect();
        assert_eq!(codes.first(), Some(&"install.dangling_symlink"), "{screen}");
        assert_eq!(codes.last(), Some(&"variant.benign_alias"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn doctor_populated_at_wide_size() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let app = doctor_app(&temporary);

        insta::assert_snapshot!(normalize_inventory(&temporary, render(&app, 120, 40)));
    }

    /// The detail region states what was observed, what it costs, the paths
    /// involved, and — because no repair exists in this release — says so
    /// rather than offering one.
    #[test]
    fn doctor_detail_drill_in_at_minimum_supported_size() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let mut app = doctor_app(&temporary);
        // The conflicting duplicate, which is the finding with the most to say.
        while app
            .selected_finding()
            .is_some_and(|entry| entry.finding().code() != "variant.duplicate_for_agent")
        {
            app.update(Action::MoveDoctorSelection(1));
        }
        app.update(Action::AdvanceDoctorPane);

        let screen = normalize_inventory(&temporary, render(&app, 80, 24));
        insta::assert_snapshot!(screen);

        // The rows a finding this long leaves below the window are reachable
        // rather than merely counted, and the last of them says what no key
        // offers: no repair exists in this release.
        scroll_detail_to_the_end(&mut app, 80, 24);
        let end = normalize_inventory(&temporary, render(&app, 80, 24));
        assert!(
            end.contains("Variant: alpha · skills · skills/review"),
            "{end}"
        );
        assert!(end.contains("Repair: not offered"), "{end}");
        assert!(!end.contains("r Repair"), "{end}");
    }

    /// The two findings that share `variant.duplicate_for_agent` state
    /// different consequences, because they are different complaints: an
    /// effective resolution does pick one definition, and a registry ambiguity
    /// picks nothing. Each must be shown beside its own evidence.
    #[test]
    fn each_duplicate_finding_states_its_own_consequence() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let mut app = doctor_app(&temporary);
        while app
            .selected_finding()
            .is_some_and(|entry| entry.agent() != AgentKind::OpenCode)
        {
            app.update(Action::MoveDoctorSelection(1));
        }
        let entry = app.selected_finding().expect("the OpenCode conflict");
        assert_eq!(
            (entry.finding().code(), entry.agent()),
            ("variant.duplicate_for_agent", AgentKind::OpenCode)
        );
        app.update(Action::AdvanceDoctorPane);

        let resolution = normalize_inventory(&temporary, render(&app, 120, 40));
        assert!(
            unwrapped(&resolution).contains(
                "The highest-precedence root wins and the other \
                                             definition is never loaded"
            ),
            "{resolution}"
        );

        // The registry-side finding of the same code, one row up.
        app.update(Action::Back);
        app.update(Action::MoveDoctorSelection(-1));
        app.update(Action::AdvanceDoctorPane);
        let registry = normalize_inventory(&temporary, render(&app, 120, 40));
        assert!(
            unwrapped(&registry).contains(
                "Which definition the agent would resolve is not something Skilled can state"
            ),
            "{registry}"
        );
    }

    /// A detail region's text with its line breaks, column padding, and the
    /// rule dividing it from the pane beside it taken out, so a sentence can be
    /// matched whole however the region happened to wrap it.
    fn unwrapped(screen: &str) -> String {
        screen
            .replace(['│', '▕', '▏'], " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Every root absent is a complete account of the roots and no reading of
    /// any of them, so Doctor may not report a clean bill of health.
    #[test]
    fn doctor_with_no_root_to_read_says_so_at_minimum_supported_size() {
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
        dispatch(&mut app, Action::OpenDoctor);

        let screen = normalize_inventory(&temporary, render(&app, 80, 24));

        assert!(!screen.contains("Nothing to report"), "{screen}");
        assert!(screen.contains("no root read"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    /// Nothing installed and nothing registered is a clean bill of health,
    /// which is not the same answer as a root that could not be read.
    #[test]
    fn doctor_empty_at_minimum_supported_size() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let home = temporary.path().join("home");
        fs::create_dir_all(home.join(".claude/skills")).expect("create an empty root");
        let mut app = SkilledApp::open(AppEnvironment::new(
            &home,
            temporary.path().join("data"),
            "",
        ))
        .expect("open application");
        for _ in 0..7 {
            dispatch(&mut app, Action::Continue);
        }
        dispatch(&mut app, Action::OpenDoctor);

        insta::assert_snapshot!(normalize_inventory(&temporary, render(&app, 80, 24)));
    }

    /// One healthy skill installed for two agents, one dangling link, and one
    /// physical copy, with the OpenCode root absent entirely.
    ///
    /// The source checkout lives inside the temporary home so every path the
    /// screen shows is home-relative and therefore stable across machines.
    fn inventory_app(temporary: &tempfile::TempDir) -> SkilledApp {
        let repository = temporary.path().join("home/library");
        for skill in ["alpha", "beta"] {
            write_skill(&repository.join("skills").join(skill), skill);
        }
        create_repository(&repository);

        let mut app = SkilledApp::open(AppEnvironment::new(
            temporary.path().join("home"),
            temporary.path().join("data"),
            "",
        ))
        .expect("open application");
        for _ in 0..7 {
            dispatch(&mut app, Action::Continue);
        }
        let preview = app.preview_source(&repository).expect("preview source");
        app.confirm_source(preview).expect("register source");

        let claude = temporary.path().join("home/.claude/skills");
        let codex = temporary.path().join("home/.agents/skills");
        fs::create_dir_all(&claude).expect("create Claude Code root");
        fs::create_dir_all(&codex).expect("create Codex root");
        link(&repository.join("skills/alpha"), &claude.join("alpha"));
        link(&temporary.path().join("home/gone"), &claude.join("broken"));
        link(&repository.join("skills/alpha"), &codex.join("alpha"));
        write_skill(&codex.join("copied"), "copied");

        app.update(Action::OpenSources);
        dispatch(&mut app, Action::OpenInventory);
        app
    }

    fn link(target: &Path, at: &Path) {
        std::os::unix::fs::symlink(target, at).expect("install symbolic link");
    }

    fn write_skill(directory: &Path, name: &str) {
        fs::create_dir_all(directory).expect("create skill fixture");
        fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {name} fixture\n---\n# {name}\n"),
        )
        .expect("write skill fixture");
    }

    fn create_repository(repository: &Path) {
        git(repository, &["init", "-b", "main"]);
        git(repository, &["config", "user.name", "Skilled Test"]);
        git(
            repository,
            &["config", "user.email", "skilled@example.test"],
        );
        git(repository, &["add", "."]);
        git(repository, &["commit", "-m", "fixture"]);
    }

    /// Temporary directories are named at random, so every path the screen
    /// shows is rewritten to a stable placeholder of the same width.
    fn normalize_inventory(temporary: &tempfile::TempDir, screen: String) -> String {
        let raw = temporary.path().to_string_lossy().into_owned();
        let canonical = temporary
            .path()
            .canonicalize()
            .expect("canonical temporary directory")
            .to_string_lossy()
            .into_owned();
        screen
            .replace(&canonical, &padded_placeholder(&canonical, "[TEMP]"))
            .replace(&raw, &padded_placeholder(&raw, "[TEMP]"))
    }
}

fn render(app: &SkilledApp, width: u16, height: u16) -> String {
    drawn(app, width, height).0
}

fn drawn(app: &SkilledApp, width: u16, height: u16) -> (String, RenderFeedback) {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("create test terminal");
    let mut feedback = RenderFeedback::default();
    terminal
        .draw(|frame| feedback = skilled::tui::render(frame, app))
        .expect("render frame");
    (buffer_text(terminal.backend().buffer()), feedback)
}

fn normalize_sources_screen(
    app: &SkilledApp,
    temporary: &tempfile::TempDir,
    screen: String,
) -> String {
    let source = &app.sources()[0];
    let temporary_path = temporary
        .path()
        .canonicalize()
        .expect("canonical temporary directory")
        .to_string_lossy()
        .into_owned();
    let repository_path = source.git_top_level().display().to_string();
    let mut normalized = screen
        .replace(
            &repository_path,
            &padded_placeholder(&repository_path, "[TEMP]/source"),
        )
        .replace(
            &temporary_path,
            &padded_placeholder(&temporary_path, "[TEMP]"),
        )
        .replace(source.head(), &padded_placeholder(source.head(), "[HEAD]"));
    // The scan time is rendered as the civil date it stands for, which is as
    // unstable as the seconds behind it. It is a fixed twenty cells wide, so
    // the placeholder keeps the layout the application produced.
    normalized = normalize_scan_timestamp(normalized);
    // The repository rows carry the abbreviated revision, which is as
    // unstable as the whole one. Replaced after the whole revision — which no
    // Sources surface states any more, though the replacement above stands
    // ready for one that does — so a region showing both is normalized as two
    // values rather than one and a fragment.
    // The placeholder is the same width as the abbreviation it stands in for,
    // so the row's columns are the ones the application laid out.
    let short_head = source.short_head();
    normalized = normalized.replace(short_head, &padded_placeholder(short_head, "[SHORT]"));
    // A path field states its value on one line and cuts it in the middle when
    // it does not fit, and a cut path is not the string these replacements
    // looked for: it would survive normalization and commit this machine's
    // temporary directory to the snapshot. The head of the path outlives any
    // such cut, so finding it here means a fixture has outgrown the region and
    // says so, rather than leaving a mystery diff on another machine.
    let root = &temporary_path[..temporary_path.len().min(10)];
    assert!(
        !normalized.contains(root),
        "the fixture's temporary path was cut rather than replaced, leaving {root:?} in\n{normalized}"
    );
    // A placeholder padded to the real path's width can still leave trailing
    // whitespace at a line's end when nothing follows it, and that width is
    // one more thing that varies with the host's temporary-directory path
    // (e.g. macOS resolving `/tmp` through `/private`). Re-trimming each line
    // keeps the snapshot independent of that length rather than just its
    // characters.
    normalized
        .lines()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Replaces the `YYYY-MM-DD HH:MM UTC` the scan field states with `[SCAN]`,
/// padded to the twenty cells the timestamp occupies.
///
/// Only a timestamp the label introduces is replaced, so a date-shaped string
/// in fixture prose stays in the snapshot where it can be read.
fn normalize_scan_timestamp(screen: String) -> String {
    const SHAPE: &str = "dddd-dd-dd dd:dd UTC";
    const LABEL: &str = "Last scan: ";
    let characters = screen.chars().collect::<Vec<_>>();
    let mut normalized = String::new();
    let mut index = 0;
    while index < characters.len() {
        let window = characters.get(index..index + SHAPE.len());
        let matches = normalized.ends_with(LABEL)
            && window.is_some_and(|window| {
                window.iter().zip(SHAPE.chars()).all(|(actual, expected)| {
                    if expected == 'd' {
                        actual.is_ascii_digit()
                    } else {
                        *actual == expected
                    }
                })
            });
        if matches {
            normalized.push_str(&padded_placeholder(SHAPE, "[SCAN]"));
            index += SHAPE.len();
        } else {
            normalized.push(characters[index]);
            index += 1;
        }
    }
    normalized
}

/// An application standing on the first variant of one registered source, in a
/// temporary tree short enough that the install dialog's absolute paths fit on
/// one line at eighty columns.
///
/// They have to: spec 15 asks the preview to state exactly what is about to be
/// written, so the dialog never elides a path, and a snapshot of one folded
/// across two rows would be measuring the fixture rather than the layout.
#[cfg(unix)]
struct InstallFixture {
    _temporary: tempfile::TempDir,
    application_root: PathBuf,
}

#[cfg(unix)]
impl InstallFixture {
    fn path(&self) -> &Path {
        &self.application_root
    }
}

#[cfg(unix)]
fn install_fixture() -> (InstallFixture, SkilledApp) {
    let temporary = tempfile::Builder::new()
        .prefix("sk-")
        .tempdir_in("/tmp")
        .expect("temporary application directory");
    // The dialog shows canonical source paths beside unresolved home paths.
    // Give those spellings different final components of equal length beneath
    // a fixed-width absolute prefix, so their rendered widths do not depend on
    // whether this platform redirects `/tmp` (macOS does; Linux does not).
    const CONTAINER_PATH_BYTES: usize = 26;
    let canonical_temporary = temporary
        .path()
        .canonicalize()
        .expect("canonical temporary directory");
    let padding = CONTAINER_PATH_BYTES
        .checked_sub(canonical_temporary.as_os_str().as_encoded_bytes().len() + 1)
        .expect("the short install fixture base fits beneath the fixed-width root");
    let container = canonical_temporary.join("x".repeat(padding));
    let real = container.join("r");
    let view = container.join("v");
    fs::create_dir_all(&real).expect("create canonical application root");
    std::os::unix::fs::symlink("r", &view).expect("create unresolved application root");
    let fixture = InstallFixture {
        _temporary: temporary,
        application_root: view,
    };
    let repository = fixture.path().join("src");
    create_source_fixture(&repository);
    for root in [".claude", ".agents", ".config/opencode"] {
        fs::create_dir_all(fixture.path().join("home").join(root))
            .expect("create the root's parent");
    }
    let mut app = SkilledApp::open(AppEnvironment::new(
        fixture.path().join("home"),
        fixture.path().join("data"),
        "",
    ))
    .expect("open application");
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    for _ in 0..7 {
        dispatch(&mut app, Action::Continue);
    }
    app.update(Action::OpenSources);
    app.update(Action::AdvanceSourcesPane);
    (fixture, app)
}

/// Replace the temporary tree's own path, keeping every line the width the
/// application produced.
///
/// Both spellings are replaced, and the canonical one first: the dialog states
/// resolved variant directories and unresolved link paths side by side, and on
/// a platform where `/tmp` is itself a link the shorter spelling is a substring
/// of the longer one.
#[cfg(unix)]
fn normalize_install_screen(temporary: &InstallFixture, screen: String) -> String {
    let canonical = temporary
        .path()
        .canonicalize()
        .expect("canonical temporary directory")
        .to_string_lossy()
        .into_owned();
    let path = temporary.path().to_string_lossy().into_owned();
    screen
        .replace(&canonical, &padded_placeholder(&canonical, "[CANONICAL]"))
        .replace(&path, &padded_placeholder(&path, "[TEMP]"))
}

/// Spec 15: the preview names every target and its exact absolute path before
/// anything is written.
#[cfg(unix)]
#[test]
fn install_preview_at_minimum_supported_size() {
    let (temporary, mut app) = install_fixture();
    dispatch(&mut app, Action::BeginInstall);

    insta::assert_snapshot!(normalize_install_screen(&temporary, render(&app, 80, 24)));
    insta::assert_snapshot!(
        "install_preview_at_wide_size",
        normalize_install_screen(&temporary, render(&app, 120, 40))
    );
}

/// A blocked plan states the finding that blocks it and offers no way to
/// confirm: the footer must not hint a key the reducer would refuse.
#[cfg(unix)]
#[test]
fn install_preview_blocked_at_minimum_supported_size() {
    let (temporary, mut app) = install_fixture();
    let root = temporary.path().join("home/.agents/skills");
    fs::create_dir_all(&root).expect("create Codex root");
    fs::write(root.join("portable"), "someone else's file").expect("occupy the slot");
    dispatch(&mut app, Action::BeginInstall);

    insta::assert_snapshot!(normalize_install_screen(&temporary, render(&app, 80, 24)));
}

/// The report states each step, the receipts behind it, and what the scan taken
/// afterwards made of the links.
#[cfg(unix)]
#[test]
fn install_report_at_minimum_supported_size() {
    let (temporary, mut app) = install_fixture();
    dispatch(&mut app, Action::BeginInstall);
    // The runner draws before every key, and a confirmation waits on what the
    // frame measured. This preview fits at the size the report is snapshotted
    // at, so the measurement is that there is nothing left to scroll to.
    app.note_detail_max_scroll(drawn(&app, 120, 40).1.detail_max_scroll());
    dispatch(&mut app, Action::ConfirmInstall);

    insta::assert_snapshot!(normalize_install_screen(&temporary, render(&app, 80, 24)));
    insta::assert_snapshot!(
        "install_report_at_wide_size",
        normalize_install_screen(&temporary, render(&app, 120, 40))
    );
}

/// A step that created a skill root and then could not write the link into it
/// states the residual root, the steps it stopped, and that nothing undoes it.
#[cfg(unix)]
#[test]
fn install_report_with_a_residual_root_at_minimum_supported_size() {
    let (temporary, mut app) = install_fixture();
    if !deny_writes_in_new_children(&temporary.path().join("home/.claude")) {
        // Nothing here can be snapshotted over a link that was written after
        // all, and a screen showing a successful install would pin the wrong
        // report under this name.
        return;
    }
    dispatch(&mut app, Action::BeginInstall);
    // The preview is the same one the successful report's test measures, so the
    // measurement is again that there is nothing left to scroll to.
    app.note_detail_max_scroll(drawn(&app, 120, 40).1.detail_max_scroll());
    dispatch(&mut app, Action::ConfirmInstall);

    insta::assert_snapshot!(normalize_install_screen(&temporary, render(&app, 80, 24)));
    // The narrow screen scrolls, so the note that nothing undoes the residual
    // root is only whole at the wider size.
    insta::assert_snapshot!(
        "install_report_with_a_residual_root_at_wide_size",
        normalize_install_screen(&temporary, render(&app, 120, 40))
    );
}

/// Deny writes inside directories created beneath `parent`, leaving `parent`
/// itself writable, and report whether the denial took effect.
///
/// A residual root exists only in the window between Skilled creating a skill
/// root and failing to write the link into it, and nothing outside the process
/// can reach into that window: the one lever on the new directory is the
/// permission it is born with. An inheritable access-control entry sets that
/// permission on the children alone, which the process umask — shared with
/// every test running beside this one — could not do safely.
///
/// The entry is proved against a probe rather than assumed, because a
/// filesystem without inheritable access control cannot stage this at all and
/// a caller told `false` has to say so rather than pin a screen that shows
/// something else.
#[cfg(unix)]
fn deny_writes_in_new_children(parent: &Path) -> bool {
    let applied = if cfg!(target_os = "macos") {
        Command::new("chmod")
            .arg("+a")
            .arg("everyone deny add_file,delete_child,file_inherit,directory_inherit,only_inherit")
            .arg(parent)
            .status()
    } else {
        Command::new("setfacl")
            .args(["-d", "-m", "u::rx,g::rx,o::rx"])
            .arg(parent)
            .status()
    };
    if !applied.is_ok_and(|status| status.success()) {
        return false;
    }
    let probe = parent.join("inheritance-probe");
    fs::create_dir(&probe).expect("create the inheritance probe");
    let denied = std::os::unix::fs::symlink(parent, probe.join("link")).is_err();
    if !denied {
        fs::remove_file(probe.join("link")).expect("remove the probe link");
    }
    fs::remove_dir(&probe).expect("remove the inheritance probe");
    denied
}

fn padded_placeholder(value: &str, placeholder: &str) -> String {
    format!(
        "{placeholder}{}",
        " ".repeat(value.len().saturating_sub(placeholder.len()))
    )
}

fn normalize_snapshot_field(screen: String, label: &str, placeholder: &str) -> String {
    screen
        .lines()
        .map(|line| {
            let Some(value_start) = line.find(label).map(|index| index + label.len()) else {
                return line.to_owned();
            };
            let Some(value_end) = line[value_start..]
                .rfind("  │")
                .map(|index| value_start + index)
            else {
                return line.to_owned();
            };
            format!(
                "{}{}{}",
                &line[..value_start],
                padded_placeholder(&line[value_start..value_end], placeholder),
                &line[value_end..]
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
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
