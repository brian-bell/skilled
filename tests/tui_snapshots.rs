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
    let missing = temporary
        .path()
        .join("a-deliberately-long-missing-repository-directory")
        .join("and-an-equally-long-nested-path");
    for character in missing.to_string_lossy().chars() {
        app.update(Action::AppendSourcePath(character));
    }
    dispatch(&mut app, Action::SubmitSourcePath);

    let temporary_path = temporary.path().to_string_lossy().into_owned();
    let rendered = render(&app, 80, 24).replace(
        &temporary_path,
        &padded_placeholder(&temporary_path, "[TEMP]"),
    );
    insta::assert_snapshot!(rendered);
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
    let temporary_path = temporary
        .path()
        .canonicalize()
        .expect("canonical temporary directory")
        .to_string_lossy()
        .into_owned();
    let short_head = &preview.inspected().head()[..8];
    let rendered = render(&app, 80, 24)
        .replace(
            &temporary_path,
            &padded_placeholder(&temporary_path, "[TEMP]"),
        )
        .replace(short_head, &padded_placeholder(short_head, "[HEAD]"));
    insta::assert_snapshot!(rendered);

    let rendered = render(&app, 120, 40)
        .replace(
            &temporary_path,
            &padded_placeholder(&temporary_path, "[TEMP]"),
        )
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
    assert!(screen.contains("Compatibility: Claude Code: yes ·"));
    assert!(screen.contains("Codex: yes · OpenCode: no"));
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
    // row deep in the list does not repeat the path it sits in.
    assert!(!variants.contains("(skills/"), "{variants}");

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

        let screen = normalize_inventory(&temporary, render(&app, 100, 26));

        // Scoped to the table's heading row: the detail region beside it has a
        // SOURCE section of its own, which is exactly where the dropped column
        // still names the source.
        assert!(!heading_row(&screen).contains("SOURCE"), "{screen}");
        assert!(screen.contains("unman"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    /// On a very wide terminal the identity columns stop growing, so a short
    /// label is not stranded in the middle of a very wide field. The slack
    /// falls to the right of Health, which is where this departs from the
    /// prototype: that grid grows these columns without bound.
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

    /// The table side of the heading row, cut at the detail region's
    /// separator so nothing the detail pane happens to render can answer for
    /// the table's columns.
    fn heading_row(screen: &str) -> &str {
        let row = screen
            .lines()
            .find(|line| line.contains("HEALTH"))
            .unwrap_or_else(|| panic!("no heading row in\n{screen}"));
        row.split('│').next().unwrap_or(row)
    }

    /// The screen column a heading starts at, counted in characters because
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
    /// pane that simply ends mid-section reads as though there were no more.
    #[test]
    fn inventory_detail_too_tall_for_the_region_reports_what_it_dropped() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let mut app = inventory_app(&temporary);
        app.update(Action::AdvanceInventoryPane);

        let screen = normalize_inventory(&temporary, render(&app, 80, 24));
        assert!(
            screen.contains("! 7 more lines — widen or lengthen the terminal"),
            "{screen}"
        );
        insta::assert_snapshot!(screen);

        // The detail region is only thirty-seven cells wide at the breakpoint,
        // so the notice takes its short form rather than wrapping off the
        // bottom — the one line whose job is to report a cut must not be cut.
        let narrow = normalize_inventory(&temporary, render(&app, 100, 26));
        assert!(narrow.contains("! 5 more lines"), "{narrow}");
        assert!(!narrow.contains("widen or lengthen"), "{narrow}");
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
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("create test terminal");
    terminal
        .draw(|frame| skilled::tui::render(frame, app))
        .expect("render frame");
    buffer_text(terminal.backend().buffer())
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
        .replace(source.head(), &padded_placeholder(source.head(), "[HEAD]"))
        .replace(
            &source.last_scan_at().to_string(),
            &padded_placeholder(&source.last_scan_at().to_string(), "[SCAN]"),
        );
    // A detail region narrower than the revision wraps it, and where the wrap
    // falls follows the region's width, so the split is searched for rather
    // than assumed. Only splits long enough to be unmistakably part of a
    // revision are considered.
    const SHORTEST_RECOGNISABLE_SPLIT: usize = 8;
    let head = source.head();
    if !normalized.contains(head) && head.len() > SHORTEST_RECOGNISABLE_SPLIT {
        for split in (SHORTEST_RECOGNISABLE_SPLIT..head.len()).rev() {
            let (wrapped, remainder) = head.split_at(split);
            if normalized.contains(wrapped) {
                normalized = normalized
                    .replace(wrapped, &padded_placeholder(wrapped, "[HEAD]"))
                    .replace(remainder, &" ".repeat(remainder.len()));
                break;
            }
        }
    }
    // The repository rows carry the abbreviated revision, which is as
    // unstable as the whole one. Replaced after it, so a region that shows
    // the revision in full is normalized as one value rather than in two
    // pieces.
    // The placeholder is the same width as the abbreviation it stands in for,
    // so the row's columns are the ones the application laid out.
    let short_head = source.short_head();
    normalized = normalized.replace(short_head, &padded_placeholder(short_head, "[SHORT]"));
    normalized
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
