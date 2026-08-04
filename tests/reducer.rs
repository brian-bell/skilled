use std::{fs, path::Path, process::Command};

use skilled::{
    Action, AgentKind, AppEnvironment, Effect, SetupStep, SkilledApp, SourcesPane, UpdateOutcome,
    View,
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
    let details = app.update(Action::AdvanceSourcesPane);
    assert_eq!(app.sources_pane(), SourcesPane::Details);
    assert!(details.effects().is_empty());

    app.update(Action::AdvanceSourcesPane);
    assert_eq!(app.sources_pane(), SourcesPane::Details);
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
    assert_eq!(
        update.effects(),
        [Effect::PersistSetup {
            agent_selections: [true, true, true]
        }]
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
        assert!(update.effects().is_empty(), "step {expected:?}");
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
