use std::{fs, path::Path, process::Command};

use skilled::{
    Action, AgentKind, AppEnvironment, Effect, InventoryPane, SetupStep, SkilledApp, SourcesPane,
    UpdateOutcome, View,
    app::{SourceRow, variant_rows},
    inventory::InstallationHealth,
};

#[test]
fn sources_region_focus_cycles_forward_without_effects() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = app_in(&temporary);
    finish_setup(&mut app);
    app.update(Action::OpenSources);

    for expected in [
        SourcesPane::Variants,
        SourcesPane::Details,
        SourcesPane::Repositories,
    ] {
        let update = app.update(Action::MoveSourcesPane(1));
        assert_eq!(app.sources_pane(), expected);
        assert_eq!(update.outcome(), UpdateOutcome::Continue);
        assert!(update.effects().is_empty());
    }
}

#[test]
fn sources_enter_requires_a_repository_then_advances_without_wrapping() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = app_in(&temporary);
    finish_setup(&mut app);
    app.update(Action::OpenSources);

    let no_source = app.update(Action::AdvanceSourcesPane);
    assert_eq!(app.sources_pane(), SourcesPane::Repositories);
    assert!(no_source.effects().is_empty());

    app.update(Action::MoveSourcesPane(1));
    let no_source = app.update(Action::AdvanceSourcesPane);
    assert_eq!(app.sources_pane(), SourcesPane::Variants);
    assert!(no_source.effects().is_empty());

    app.update(Action::AdvanceSourcesPane);
    assert_eq!(app.sources_pane(), SourcesPane::Variants);
}

#[test]
fn sources_back_walks_the_region_hierarchy_before_leaving() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = app_in(&temporary);
    finish_setup(&mut app);
    app.update(Action::OpenSources);
    app.update(Action::MoveSourcesPane(-1));
    assert_eq!(app.sources_pane(), SourcesPane::Details);

    for expected in [SourcesPane::Variants, SourcesPane::Repositories] {
        let update = app.update(Action::Back);
        assert_eq!(app.view(), View::Sources);
        assert_eq!(app.sources_pane(), expected);
        assert!(update.effects().is_empty());
    }

    app.update(Action::Back);
    assert_eq!(app.view(), View::Inventory);
}

#[test]
fn reopening_sources_starts_at_repositories() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = app_in(&temporary);
    finish_setup(&mut app);
    app.update(Action::OpenSources);
    app.update(Action::MoveSourcesPane(-1));
    assert_eq!(app.sources_pane(), SourcesPane::Details);

    app.update(Action::OpenInventory);
    app.update(Action::OpenSources);

    assert_eq!(app.sources_pane(), SourcesPane::Repositories);
}

#[test]
fn sources_region_focus_normalizes_backward_and_large_movements() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = app_in(&temporary);
    finish_setup(&mut app);
    app.update(Action::OpenSources);

    app.update(Action::MoveSourcesPane(-1));
    assert_eq!(app.sources_pane(), SourcesPane::Details);
    app.update(Action::MoveSourcesPane(4));
    assert_eq!(app.sources_pane(), SourcesPane::Repositories);
    app.update(Action::MoveSourcesPane(-4));
    assert_eq!(app.sources_pane(), SourcesPane::Details);
}

#[test]
fn changing_repository_resets_the_variant_selection() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = app_in(&temporary);
    finish_setup(&mut app);
    register_source(&mut app, &temporary.path().join("first"), 2);
    register_source(&mut app, &temporary.path().join("second"), 2);
    app.update(Action::OpenSources);
    app.update(Action::MoveSourcesPane(1));
    app.update(Action::MoveSourcesSelection(1));
    assert_eq!(app.focused_variant(), 1);

    app.update(Action::MoveSourcesPane(-1));
    app.update(Action::MoveSourcesSelection(-1));

    assert_eq!(app.focused_source(), 0);
    assert_eq!(app.focused_variant(), 0);
}

#[test]
fn moving_a_singleton_repository_preserves_the_selected_variant() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = app_in(&temporary);
    finish_setup(&mut app);
    register_source(&mut app, &temporary.path().join("source"), 2);
    app.update(Action::OpenSources);
    app.update(Action::MoveSourcesPane(1));
    app.update(Action::MoveSourcesSelection(1));
    app.update(Action::MoveSourcesPane(-1));

    app.update(Action::MoveSourcesSelection(1));

    assert_eq!(app.focused_source(), 0);
    assert_eq!(app.focused_variant(), 1);
}

/// With no candidates anywhere, the catalog-state rows — `no variants`, or a
/// catalog's error — are the variants pane's rows, so focus moves over the
/// catalogs rather than dying at zero variants and leaving clipped catalogs
/// unreachable.
#[test]
fn an_all_empty_source_moves_its_variant_focus_over_catalog_rows() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = app_in(&temporary);
    finish_setup(&mut app);
    let repository = temporary.path().join("source");
    fs::create_dir_all(repository.join("skills")).expect("create common catalog");
    fs::write(repository.join("skills/.keep"), "empty catalog fixture").expect("write keep file");
    fs::create_dir_all(repository.join("experimental/claude-code/skills"))
        .expect("create agent catalog");
    fs::write(
        repository.join("experimental/claude-code/skills/.keep"),
        "empty catalog fixture",
    )
    .expect("write keep file");
    git(&repository, &["init", "-b", "main"]);
    git(&repository, &["config", "user.name", "Skilled Test"]);
    git(
        &repository,
        &["config", "user.email", "skilled@example.test"],
    );
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "fixture"]);
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    app.update(Action::OpenSources);
    app.update(Action::MoveSourcesPane(1));
    assert_eq!(app.focused_variant(), 0);

    app.update(Action::MoveSourcesSelection(1));
    assert_eq!(app.focused_variant(), 1);

    app.update(Action::MoveSourcesSelection(1));
    assert_eq!(app.focused_variant(), 0);
}

/// A candidate somewhere must not strand the other catalogs: the selection
/// counts every row the pane renders — candidates and catalog-state rows
/// alike — so a mixed source is walkable end to end.
#[test]
fn a_mixed_source_moves_its_focus_over_candidate_and_catalog_state_rows() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = app_in(&temporary);
    finish_setup(&mut app);
    let repository = temporary.path().join("source");
    let skill = repository.join("skills/portable");
    fs::create_dir_all(&skill).expect("create candidate");
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: portable\ndescription: Portable fixture\n---\n# Portable\n",
    )
    .expect("write candidate");
    fs::create_dir_all(repository.join("experimental/claude-code/skills"))
        .expect("create empty catalog");
    fs::write(
        repository.join("experimental/claude-code/skills/.keep"),
        "empty catalog fixture",
    )
    .expect("write keep file");
    git(&repository, &["init", "-b", "main"]);
    git(&repository, &["config", "user.name", "Skilled Test"]);
    git(
        &repository,
        &["config", "user.email", "skilled@example.test"],
    );
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "fixture"]);
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    app.update(Action::OpenSources);
    app.update(Action::MoveSourcesPane(1));
    assert_eq!(app.focused_variant(), 0);

    app.update(Action::MoveSourcesSelection(1));
    assert_eq!(app.focused_variant(), 1);

    app.update(Action::MoveSourcesSelection(1));
    assert_eq!(app.focused_variant(), 0);
}

/// The variants pane, the selection count, and the detail region all read the
/// same sequence, so the order itself is worth stating once in a test: for
/// each catalog, its scan error, then its candidates, then the `no variants`
/// line an empty catalog shows.
///
/// Asserted over a source holding all three row kinds at once, because a
/// mixture is where an order maintained in three places used to be able to
/// drift — one catalog unreadable, one holding skills, one read and empty.
#[test]
fn variant_rows_are_ordered_by_catalog_then_error_candidates_and_empty_state() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = app_in(&temporary);
    finish_setup(&mut app);
    let repository = temporary.path().join("source");
    for catalog in ["unreadable", "populated"] {
        let directory = repository.join(catalog).join("codex/skills");
        for skill in ["first", "second"] {
            let candidate = directory.join(format!("{catalog}-{skill}"));
            fs::create_dir_all(&candidate).expect("create candidate fixture");
            fs::write(
                candidate.join("SKILL.md"),
                format!(
                    "---\nname: {catalog}-{skill}\ndescription: {catalog} {skill}\n---\n# Fixture\n"
                ),
            )
            .expect("write candidate fixture");
        }
    }
    let empty = repository.join("empty/codex/skills");
    fs::create_dir_all(&empty).expect("create empty catalog fixture");
    fs::write(empty.join(".keep"), "empty catalog fixture").expect("write keep file");
    git(&repository, &["init", "-b", "main"]);
    git(&repository, &["config", "user.name", "Skilled Test"]);
    git(
        &repository,
        &["config", "user.email", "skilled@example.test"],
    );
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "fixture"]);
    let environment = AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    );
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    drop(app);
    // One catalog is made unscannable where it is stored, which is the only
    // way a registered catalog acquires a scan error: the path it names is
    // rejected on the next read.
    let connection = rusqlite::Connection::open(temporary.path().join("data/skilled.sqlite3"))
        .expect("open application database");
    connection
        .execute(
            "UPDATE catalog_roots SET relative_path = '../outside' \
             WHERE relative_path LIKE 'unreadable%'",
            [],
        )
        .expect("create unscannable stored catalog fixture");
    drop(connection);
    let mut app = SkilledApp::open(environment).expect("reopen application");
    app.update(Action::OpenSources);

    let source = app.selected_source().expect("registered source");
    let rows = variant_rows(source).map(describe_row).collect::<Vec<_>>();

    assert_eq!(
        rows,
        vec![
            "../outside: scan error".to_owned(),
            "empty/codex/skills: no variants".to_owned(),
            "populated/codex/skills: populated-first".to_owned(),
            "populated/codex/skills: populated-second".to_owned(),
        ]
    );
    // The three readers of that order agree by construction, and this is what
    // says so: the count the selection wraps at, and the row the detail region
    // resolves at every position, are the same sequence.
    assert_eq!(app.variants_row_count(), rows.len());
    app.update(Action::MoveSourcesPane(1));
    for expected in &rows {
        assert_eq!(
            app.selected_variant_row().map(describe_row).as_ref(),
            Some(expected)
        );
        app.update(Action::MoveSourcesSelection(1));
    }
}

/// A row as `catalog path: what the row says` — the error's catalog, the
/// candidate's directory name, or the empty state.
fn describe_row(row: SourceRow<'_>) -> String {
    let catalog = row.catalog().relative_path().display().to_string();
    match row {
        SourceRow::CatalogError(catalog_proposal) => {
            catalog_proposal
                .scan_error()
                .expect("an error row states its error");
            format!("{catalog}: scan error")
        }
        SourceRow::Variant { candidate, .. } => {
            format!("{catalog}: {}", candidate.directory_name())
        }
        SourceRow::NoVariants(_) => format!("{catalog}: no variants"),
    }
}

#[test]
fn sources_enter_opens_details_for_a_selected_source_with_no_variants() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = app_in(&temporary);
    finish_setup(&mut app);
    register_source(&mut app, &temporary.path().join("source"), 0);
    app.update(Action::OpenSources);

    app.update(Action::AdvanceSourcesPane);
    assert_eq!(app.sources_pane(), SourcesPane::Variants);
    app.update(Action::AdvanceSourcesPane);

    assert_eq!(app.sources_pane(), SourcesPane::Details);
}

#[test]
fn details_focus_preserves_repository_and_variant_selection() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = app_in(&temporary);
    finish_setup(&mut app);
    register_source(&mut app, &temporary.path().join("source"), 2);
    app.update(Action::OpenSources);
    app.update(Action::MoveSourcesPane(1));
    app.update(Action::MoveSourcesSelection(1));
    app.update(Action::MoveSourcesPane(1));

    app.update(Action::MoveSourcesSelection(1));

    assert_eq!(app.focused_source(), 0);
    assert_eq!(app.focused_variant(), 1);
}

#[test]
fn sources_enter_opens_variants_when_a_repository_is_selected() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = app_in(&temporary);
    finish_setup(&mut app);
    register_source(&mut app, &temporary.path().join("source"), 1);
    app.update(Action::OpenSources);

    let update = app.update(Action::AdvanceSourcesPane);

    assert_eq!(app.sources_pane(), SourcesPane::Variants);
    assert!(update.effects().is_empty());
}

#[test]
fn setup_actions_advance_and_change_the_focused_agent_selection() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");

    assert_eq!(
        app.update(Action::Continue).outcome(),
        UpdateOutcome::Continue
    );
    assert_eq!(app.view(), View::Setup(SetupStep::DetectAgents));

    app.update(Action::MoveSelection(1));
    app.update(Action::ToggleSelection);

    assert!(app.agent(AgentKind::ClaudeCode).selected());
    assert!(!app.agent(AgentKind::Codex).selected());
    assert!(app.agent(AgentKind::OpenCode).selected());
}

#[test]
fn finishing_setup_returns_a_persistence_effect_without_writing_metadata() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let environment = AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    );
    let mut app = SkilledApp::open(environment.clone()).expect("open application");
    for _ in 0..6 {
        app.update(Action::Continue);
    }

    let update = app.update(Action::Continue);

    assert_eq!(update.outcome(), UpdateOutcome::Continue);
    // Arriving in the Inventory restates what is installed; persistence is the
    // only effect that writes anything.
    assert_eq!(
        update.effects(),
        [
            Effect::ScanInstallations,
            Effect::PersistSetup {
                agent_selections: [true, true, true]
            }
        ]
    );
    drop(app);
    assert_eq!(
        SkilledApp::open(environment)
            .expect("reopen without executing effect")
            .view(),
        View::Setup(SetupStep::Welcome)
    );
}

#[test]
fn back_is_a_no_op_on_the_first_setup_step() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");

    let update = app.update(Action::Back);

    assert_eq!(update.outcome(), UpdateOutcome::Continue);
    assert!(update.effects().is_empty());
    assert_eq!(app.view(), View::Setup(SetupStep::Welcome));
}

#[test]
fn placeholder_setup_steps_advance_without_external_effects() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");

    for expected in [
        SetupStep::DetectAgents,
        SetupStep::ChooseScanRoots,
        SetupStep::DiscoverSources,
        SetupStep::ConfirmCatalogs,
        SetupStep::ScanInstallations,
        SetupStep::Summary,
    ] {
        let update = app.update(Action::Continue);
        // Step six reads the installation roots, and reading them is the step;
        // every other step still advances on its own.
        let expected_effects: &[Effect] = if expected == SetupStep::ScanInstallations {
            &[Effect::ScanInstallations]
        } else {
            &[]
        };
        assert_eq!(update.effects(), expected_effects, "step {expected:?}");
        assert_eq!(app.view(), View::Setup(expected));
    }
}

#[test]
fn settings_rerun_emits_only_reset_and_redetection_in_order() {
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
    for _ in 0..6 {
        let update = app.update(Action::Continue);
        app.perform_effects(update.effects())
            .expect("complete setup");
    }
    app.update(Action::OpenSettings);

    let update = app.update(Action::RerunSetup);

    assert_eq!(app.view(), View::Setup(SetupStep::Welcome));
    assert_eq!(
        update.effects(),
        [
            Effect::ResetSetup,
            Effect::RedetectAgents {
                agent_selections: [true, false, true],
            },
        ]
    );
}

#[test]
fn help_captures_and_protects_every_implemented_top_level_context() {
    let setup_directory = tempfile::tempdir().expect("temporary application directory");
    let mut setup = app_in(&setup_directory);
    setup.update(Action::Continue);
    assert_help_blocks(&mut setup, Action::Continue);

    let inventory_directory = tempfile::tempdir().expect("temporary application directory");
    let mut inventory = app_in(&inventory_directory);
    finish_setup(&mut inventory);
    assert_help_blocks(&mut inventory, Action::OpenSources);

    let sources_directory = tempfile::tempdir().expect("temporary application directory");
    let mut sources = app_in(&sources_directory);
    finish_setup(&mut sources);
    sources.update(Action::OpenSources);
    assert_help_blocks(&mut sources, Action::Back);

    let settings_directory = tempfile::tempdir().expect("temporary application directory");
    let mut settings = app_in(&settings_directory);
    finish_setup(&mut settings);
    settings.update(Action::OpenSettings);
    assert_help_blocks(&mut settings, Action::RerunSetup);
}

#[test]
fn help_does_not_open_over_source_path_entry() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    for _ in 0..3 {
        app.update(Action::Continue);
    }
    app.update(Action::BeginAddSource);
    assert!(app.source_path_input_active());

    app.update(Action::OpenHelp);

    assert_eq!(app.help_context(), None);
    assert!(app.source_path_input_active());
}

#[test]
fn sources_add_flow_collects_a_path_before_requesting_inspection() {
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
    for character in "/tmp/source ".chars() {
        app.update(Action::AppendSourcePath(character));
    }
    let update = app.update(Action::SubmitSourcePath);

    assert_eq!(app.view(), View::Sources);
    assert!(app.source_path_input_active());
    assert_eq!(
        update.effects(),
        [Effect::InspectSource {
            path: "/tmp/source ".into()
        }]
    );
}

#[test]
fn empty_source_path_submission_is_an_effect_free_no_op() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = app_in(&temporary);
    finish_setup(&mut app);
    app.update(Action::OpenSources);
    app.update(Action::BeginAddSource);
    for character in "   ".chars() {
        app.update(Action::AppendSourcePath(character));
    }

    let update = app.update(Action::SubmitSourcePath);

    assert!(update.effects().is_empty());
    assert!(app.source_path_input_active());
    assert_eq!(app.source_path(), "   ");
    assert_eq!(app.view(), View::Sources);
}

#[test]
fn entering_the_inventory_view_requests_a_fresh_installation_scan() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = app_in(&temporary);
    finish_setup(&mut app);
    app.update(Action::OpenSources);

    let update = app.update(Action::OpenInventory);

    assert_eq!(app.view(), View::Inventory);
    assert_eq!(update.effects(), [Effect::ScanInstallations]);
}

#[test]
fn leaving_sources_or_settings_rescans_on_the_way_back_to_inventory() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = app_in(&temporary);
    finish_setup(&mut app);

    app.update(Action::OpenSources);
    let from_sources = app.update(Action::Back);
    assert_eq!(app.view(), View::Inventory);
    assert_eq!(from_sources.effects(), [Effect::ScanInstallations]);

    app.update(Action::OpenSettings);
    let from_settings = app.update(Action::Back);
    assert_eq!(app.view(), View::Inventory);
    assert_eq!(from_settings.effects(), [Effect::ScanInstallations]);
}

#[test]
fn the_scan_installations_step_and_the_end_of_setup_both_request_a_scan() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = app_in(&temporary);

    // Steps one through four announce nothing; step six is where the roots are
    // read, so the scan is requested on arrival there.
    for _ in 0..4 {
        assert!(app.update(Action::Continue).effects().is_empty());
    }
    let arriving = app.update(Action::Continue);
    assert_eq!(app.view(), View::Setup(SetupStep::ScanInstallations));
    assert_eq!(arriving.effects(), [Effect::ScanInstallations]);

    assert!(app.update(Action::Continue).effects().is_empty());
    let finishing = app.update(Action::Continue);
    assert_eq!(app.view(), View::Inventory);
    assert_eq!(
        finishing.effects(),
        [
            Effect::ScanInstallations,
            Effect::PersistSetup {
                agent_selections: [true; 3]
            }
        ]
    );
}

#[test]
fn inventory_region_focus_cycles_and_enter_requires_a_selected_row() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = app_in(&temporary);
    finish_setup(&mut app);
    assert_eq!(app.inventory_pane(), InventoryPane::Skills);

    // Nothing is installed, so there is nothing to drill into.
    app.update(Action::AdvanceInventoryPane);
    assert_eq!(app.inventory_pane(), InventoryPane::Skills);

    for expected in [InventoryPane::Details, InventoryPane::Skills] {
        let update = app.update(Action::MoveInventoryPane(1));
        assert_eq!(app.inventory_pane(), expected);
        assert!(update.effects().is_empty());
    }
    app.update(Action::MoveInventoryPane(-1));
    assert_eq!(app.inventory_pane(), InventoryPane::Details);
}

#[test]
fn an_empty_inventory_cannot_open_the_filter_bar() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = app_in(&temporary);
    finish_setup(&mut app);

    app.update(Action::BeginInventoryFilter);

    assert!(!app.inventory_filter_active());
}

fn app_in(directory: &tempfile::TempDir) -> SkilledApp {
    SkilledApp::open(AppEnvironment::new(
        directory.path().join("home"),
        directory.path().join("data"),
        "",
    ))
    .expect("open application")
}

fn finish_setup(app: &mut SkilledApp) {
    for _ in 0..7 {
        let update = app.update(Action::Continue);
        app.perform_effects(update.effects())
            .expect("setup effects");
    }
}

fn assert_help_blocks(app: &mut SkilledApp, blocked_action: Action) {
    let original_view = app.view();

    let opened = app.update(Action::OpenHelp);
    assert_eq!(opened.outcome(), UpdateOutcome::Continue);
    assert!(opened.effects().is_empty());
    assert_eq!(app.help_context(), Some(original_view));
    assert_eq!(app.view(), original_view);

    let blocked = app.update(blocked_action);
    assert_eq!(blocked.outcome(), UpdateOutcome::Continue);
    assert!(blocked.effects().is_empty());
    assert_eq!(app.help_context(), Some(original_view));
    assert_eq!(app.view(), original_view);

    let closed = app.update(Action::CloseHelp);
    assert_eq!(closed.outcome(), UpdateOutcome::Continue);
    assert!(closed.effects().is_empty());
    assert_eq!(app.help_context(), None);
    assert_eq!(app.view(), original_view);
}

fn register_source(app: &mut SkilledApp, repository: &Path, variants: usize) {
    fs::create_dir_all(repository.join("skills")).expect("create catalog fixture");
    if variants == 0 {
        fs::write(repository.join("skills/.keep"), "empty catalog fixture")
            .expect("write empty catalog fixture");
    }
    for index in 0..variants {
        let skill = repository.join("skills").join(format!("variant-{index}"));
        fs::create_dir_all(&skill).expect("create skill fixture");
        fs::write(
            skill.join("SKILL.md"),
            format!(
                "---\nname: variant-{index}\ndescription: Variant {index}\n---\n# Variant {index}\n"
            ),
        )
        .expect("write skill fixture");
    }
    git(repository, &["init", "-b", "main"]);
    git(repository, &["config", "user.name", "Skilled Test"]);
    git(
        repository,
        &["config", "user.email", "skilled@example.test"],
    );
    git(repository, &["add", "."]);
    git(repository, &["commit", "-m", "fixture"]);
    let preview = app.preview_source(repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
}

fn git(repository: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .expect("run Git fixture command");
    assert!(output.status.success(), "Git command failed: {output:?}");
}

/// Reducer behaviour that needs a real installed skill.
///
/// Managed installations are symbolic links, so these fixtures are
/// Unix-only.
#[cfg(unix)]
mod installed {
    use super::*;

    #[test]
    fn registering_a_source_restates_the_inventory_without_a_separate_scan() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let repository = temporary.path().join("library");
        let mut app = app_in(&temporary);
        finish_setup(&mut app);
        write_skill_fixture(&repository.join("skills/variant-0"), "variant-0");
        install_link(
            &temporary,
            AgentKind::ClaudeCode,
            "variant-0",
            &repository.join("skills/variant-0"),
        );

        // Before the source is registered the link is a foreign one; registering
        // the checkout it points into is what makes it managed.
        app.update(Action::OpenSources);
        let update = app.update(Action::OpenInventory);
        app.perform_effects(update.effects()).expect("scan");
        assert_eq!(
            app.inventory().row("variant-0").map(|row| row.health()),
            Some(InstallationHealth::Unmanaged)
        );

        register_source(&mut app, &repository, 1);

        assert_eq!(
            app.inventory().row("variant-0").map(|row| row.health()),
            Some(InstallationHealth::Healthy)
        );
        assert_eq!(
            app.inventory().row("variant-0").map(|row| row.provenance()),
            Some(skilled::inventory::RowProvenance::Source("library"))
        );
    }
    #[test]
    fn inventory_selection_wraps_within_the_rows_the_filter_admits() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let mut app = inventory_app(&temporary);

        for expected in [1, 2, 0] {
            let update = app.update(Action::MoveInventorySelection(1));
            assert_eq!(app.focused_installation(), expected);
            assert!(update.effects().is_empty());
        }
        app.update(Action::MoveInventorySelection(-1));
        assert_eq!(app.focused_installation(), 2);
    }
    #[test]
    fn enter_drills_into_details_when_a_row_is_selected() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let mut app = inventory_app(&temporary);

        app.update(Action::AdvanceInventoryPane);

        assert_eq!(app.inventory_pane(), InventoryPane::Details);
        // Selection only moves in the list region.
        app.update(Action::MoveInventorySelection(1));
        assert_eq!(app.focused_installation(), 0);
    }
    #[test]
    fn the_filter_narrows_by_name_source_and_health() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let mut app = inventory_app(&temporary);
        assert_eq!(app.filtered_rows().len(), 3);

        type_filter(&mut app, "variant-1");
        assert_eq!(names(&app), ["variant-1"]);

        clear_filter(&mut app);
        type_filter(&mut app, "library");
        assert_eq!(names(&app), ["variant-0", "variant-1"]);

        clear_filter(&mut app);
        type_filter(&mut app, "unmanaged");
        assert_eq!(names(&app), ["foreign"]);
    }
    /// A row that mixes a managed installation with an unmanaged one still
    /// holds an installation that resolved to no registered source, so the
    /// provenance query that surfaces unmanaged installations must admit it.
    #[test]
    fn the_filter_surfaces_the_unmanaged_installation_inside_a_mixed_row() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let repository = temporary.path().join("library");
        let mut app = app_in(&temporary);
        finish_setup(&mut app);
        register_source(&mut app, &repository, 1);
        install_link(
            &temporary,
            AgentKind::ClaudeCode,
            "variant-0",
            &repository.join("skills/variant-0"),
        );
        write_skill_fixture(
            &temporary.path().join("home/.agents/skills/variant-0"),
            "variant-0",
        );
        let update = app.update(Action::OpenSources);
        app.perform_effects(update.effects()).expect("effects");
        let update = app.update(Action::OpenInventory);
        app.perform_effects(update.effects())
            .expect("installation scan");
        assert_eq!(
            app.inventory().row("variant-0").map(|row| row.provenance()),
            Some(skilled::inventory::RowProvenance::Mixed)
        );

        type_filter(&mut app, "not registered");
        assert_eq!(names(&app), ["variant-0"]);
    }
    /// Stray content is not an installation, so the provenance query must not
    /// present it as content that came from nowhere registered.
    #[test]
    fn the_filter_does_not_present_stray_content_as_not_registered() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let repository = temporary.path().join("library");
        let mut app = app_in(&temporary);
        finish_setup(&mut app);
        register_source(&mut app, &repository, 1);
        install_link(
            &temporary,
            AgentKind::ClaudeCode,
            "variant-0",
            &repository.join("skills/variant-0"),
        );
        fs::write(
            temporary.path().join("home/.claude/skills/readme.md"),
            "not a skill",
        )
        .expect("write stray file");
        let update = app.update(Action::OpenSources);
        app.perform_effects(update.effects()).expect("effects");
        let update = app.update(Action::OpenInventory);
        app.perform_effects(update.effects())
            .expect("installation scan");

        type_filter(&mut app, "not registered");
        assert_eq!(names(&app), Vec::<String>::new());
    }
    /// The query box is drawn above the table, so a compact terminal showing
    /// only the detail region has nowhere to draw it — and a field the user
    /// cannot see must not take every printable key.
    #[test]
    fn the_detail_region_cannot_open_a_filter_it_has_nowhere_to_show() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let mut app = inventory_app(&temporary);
        app.update(Action::AdvanceInventoryPane);
        assert_eq!(app.inventory_pane(), InventoryPane::Details);
        assert!(!app.can_filter_inventory());

        app.update(Action::BeginInventoryFilter);

        assert!(!app.inventory_filter_active());
        assert_eq!(app.inventory_filter(), "");

        // Back in the list region it opens as usual.
        app.update(Action::Back);
        assert!(app.can_filter_inventory());
        app.update(Action::BeginInventoryFilter);
        assert!(app.inventory_filter_active());
    }

    #[test]
    fn the_filter_bar_owns_the_keyboard_while_it_is_open() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let mut app = inventory_app(&temporary);
        app.update(Action::BeginInventoryFilter);
        assert!(app.inventory_filter_active());

        // Navigation cannot fire out from under a half-typed query.
        app.update(Action::OpenSources);
        app.update(Action::OpenSettings);
        assert_eq!(app.view(), View::Inventory);

        // Quitting is the one command no context may swallow.
        assert_eq!(app.update(Action::Quit).outcome(), UpdateOutcome::Quit);
    }
    #[test]
    fn submitting_keeps_the_query_and_cancelling_clears_it() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let mut app = inventory_app(&temporary);

        type_filter(&mut app, "variant");
        assert!(!app.inventory_filter_active());
        assert_eq!(app.inventory_filter(), "variant");
        assert_eq!(names(&app), ["variant-0", "variant-1"]);

        // Back unwinds the applied query before anything else.
        app.update(Action::Back);
        assert_eq!(app.inventory_filter(), "");
        assert_eq!(app.filtered_rows().len(), 3);

        app.update(Action::BeginInventoryFilter);
        app.update(Action::AppendInventoryFilter('f'));
        app.update(Action::DeleteInventoryFilterCharacter);
        app.update(Action::Back);
        assert_eq!(app.inventory_filter(), "");
        assert!(!app.inventory_filter_active());
    }
    #[test]
    fn a_narrowing_filter_pulls_the_selection_back_into_range() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let mut app = inventory_app(&temporary);
        app.update(Action::MoveInventorySelection(-1));
        assert_eq!(app.focused_installation(), 2);

        type_filter(&mut app, "variant-0");

        assert_eq!(app.focused_installation(), 0);
        assert_eq!(
            app.selected_installation().map(|row| row.name().to_owned()),
            Some("variant-0".to_owned())
        );
    }
    /// An application whose Inventory holds two managed variants and one foreign
    /// installation, focused on the first row.
    fn inventory_app(temporary: &tempfile::TempDir) -> SkilledApp {
        let repository = temporary.path().join("library");
        let mut app = app_in(temporary);
        finish_setup(&mut app);
        register_source(&mut app, &repository, 2);
        for index in 0..2 {
            install_link(
                temporary,
                AgentKind::ClaudeCode,
                &format!("variant-{index}"),
                &repository.join("skills").join(format!("variant-{index}")),
            );
        }
        let foreign = temporary.path().join("elsewhere/foreign");
        write_skill_fixture(&foreign, "foreign");
        install_link(temporary, AgentKind::ClaudeCode, "foreign", &foreign);

        let update = app.update(Action::OpenSources);
        app.perform_effects(update.effects()).expect("effects");
        let update = app.update(Action::OpenInventory);
        app.perform_effects(update.effects())
            .expect("installation scan");
        app
    }
    #[test]
    fn returning_to_the_inventory_keeps_the_focused_row() {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        write_skill_fixture(&temporary.path().join("home/.claude/skills/alpha"), "alpha");
        write_skill_fixture(&temporary.path().join("home/.claude/skills/beta"), "beta");
        let mut app = app_in(&temporary);
        finish_setup(&mut app);
        app.update(Action::MoveInventorySelection(1));
        let before = app.selected_installation().map(|row| row.name().to_owned());
        assert!(before.is_some());

        app.update(Action::OpenSources);
        let update = app.update(Action::OpenInventory);
        app.perform_effects(update.effects())
            .expect("installation scan");

        // The gap reset must not clamp the selection away: the row the user
        // was on survives the leave-and-rescan round trip.
        assert_eq!(
            app.selected_installation().map(|row| row.name()),
            before.as_deref()
        );
    }

    fn write_skill_fixture(directory: &Path, name: &str) {
        fs::create_dir_all(directory).expect("create skill fixture directory");
        fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {name} fixture\n---\n# {name}\n"),
        )
        .expect("write skill fixture");
    }
    fn install_link(temporary: &tempfile::TempDir, agent: AgentKind, name: &str, target: &Path) {
        let root = temporary.path().join("home").join(match agent {
            AgentKind::ClaudeCode => ".claude/skills",
            AgentKind::Codex => ".agents/skills",
            AgentKind::OpenCode => ".config/opencode/skills",
        });
        fs::create_dir_all(&root).expect("create agent skill root");
        std::os::unix::fs::symlink(target, root.join(name)).expect("install symbolic link");
    }
    fn type_filter(app: &mut SkilledApp, query: &str) {
        app.update(Action::BeginInventoryFilter);
        for character in query.chars() {
            app.update(Action::AppendInventoryFilter(character));
        }
        app.update(Action::SubmitInventoryFilter);
    }
    fn clear_filter(app: &mut SkilledApp) {
        app.update(Action::Back);
    }
    fn names(app: &SkilledApp) -> Vec<String> {
        app.filtered_rows()
            .iter()
            .map(|row| row.name().to_owned())
            .collect()
    }
}
