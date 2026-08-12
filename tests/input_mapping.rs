use std::{fs, path::Path, process::Command};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use skilled::{Action, AppEnvironment, SetupStep, SkilledApp, View};

#[test]
fn keys_map_to_contextual_actions_without_mutating_application_state() {
    use skilled::input::action_for_key;

    assert_eq!(
        action_for_key(View::Setup(SetupStep::Welcome), key(KeyCode::Enter)),
        Some(Action::Continue)
    );
    assert_eq!(
        action_for_key(View::Setup(SetupStep::DetectAgents), key(KeyCode::Down)),
        Some(Action::MoveSelection(1))
    );
    assert_eq!(
        action_for_key(
            View::Setup(SetupStep::DetectAgents),
            key(KeyCode::Char(' '))
        ),
        Some(Action::ToggleSelection)
    );
    assert_eq!(
        action_for_key(View::Inventory, key(KeyCode::Char('s'))),
        Some(Action::OpenSettings)
    );
    assert_eq!(
        action_for_key(View::Settings, key(KeyCode::Enter)),
        Some(Action::RerunSetup)
    );
    assert_eq!(
        action_for_key(View::Settings, key(KeyCode::Esc)),
        Some(Action::Back)
    );
    assert_eq!(
        action_for_key(View::Inventory, key(KeyCode::Char('q'))),
        Some(Action::Quit)
    );
    assert_eq!(
        action_for_key(
            View::Inventory,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
        ),
        Some(Action::Quit)
    );
}

#[test]
fn sources_tab_and_backtab_move_region_focus_in_opposite_directions() {
    use skilled::input::action_for_key;

    assert_eq!(
        action_for_key(View::Sources, key(KeyCode::Tab)),
        Some(Action::MoveSourcesPane(1))
    );
    assert_eq!(
        action_for_key(View::Sources, key(KeyCode::BackTab)),
        Some(Action::MoveSourcesPane(-1))
    );
}

#[test]
fn sources_enter_advances_and_escape_backs_through_the_region_hierarchy() {
    use skilled::input::action_for_key;

    assert_eq!(
        action_for_key(View::Sources, key(KeyCode::Enter)),
        Some(Action::AdvanceSourcesPane)
    );
    assert_eq!(
        action_for_key(View::Sources, key(KeyCode::Esc)),
        Some(Action::Back)
    );
}

#[test]
fn inventory_navigates_its_rows_and_regions() {
    use skilled::input::action_for_key;

    for (code, expected) in [
        (KeyCode::Tab, Action::MoveInventoryPane(1)),
        (KeyCode::BackTab, Action::MoveInventoryPane(-1)),
        (KeyCode::Enter, Action::AdvanceInventoryPane),
        (KeyCode::Up, Action::MoveInventorySelection(-1)),
        (KeyCode::Char('k'), Action::MoveInventorySelection(-1)),
        (KeyCode::Down, Action::MoveInventorySelection(1)),
        (KeyCode::Char('j'), Action::MoveInventorySelection(1)),
        (KeyCode::Char('/'), Action::BeginInventoryFilter),
        (KeyCode::Esc, Action::Back),
        (KeyCode::Char('2'), Action::OpenSources),
    ] {
        assert_eq!(
            action_for_key(View::Inventory, key(code)),
            Some(expected),
            "{code:?}"
        );
    }
}

/// Doctor handles exactly the keys its hints and its help entry advertise: a
/// hint that no mapping backs is a promise the application cannot keep.
#[test]
fn doctor_navigates_its_findings_regions_and_routes() {
    use skilled::input::action_for_key;

    for (code, expected) in [
        (KeyCode::Tab, Action::MoveDoctorPane(1)),
        (KeyCode::BackTab, Action::MoveDoctorPane(-1)),
        (KeyCode::Enter, Action::AdvanceDoctorPane),
        (KeyCode::Up, Action::MoveDoctorSelection(-1)),
        (KeyCode::Char('k'), Action::MoveDoctorSelection(-1)),
        (KeyCode::Down, Action::MoveDoctorSelection(1)),
        (KeyCode::Char('j'), Action::MoveDoctorSelection(1)),
        (KeyCode::Esc, Action::Back),
        (KeyCode::Char('1'), Action::OpenInventory),
        (KeyCode::Char('2'), Action::OpenSources),
        (KeyCode::Char('q'), Action::Quit),
    ] {
        assert_eq!(
            action_for_key(View::Doctor, key(code)),
            Some(expected),
            "{code:?}"
        );
    }

    // Doctor is already on screen, and Updates has no implementation, so
    // neither digit is bound here.
    for code in [KeyCode::Char('3'), KeyCode::Char('4')] {
        assert_eq!(action_for_key(View::Doctor, key(code)), None, "{code:?}");
    }

    // The route the other two workspaces offer into Doctor.
    for view in [View::Inventory, View::Sources] {
        assert_eq!(
            action_for_key(view, key(KeyCode::Char('4'))),
            Some(Action::OpenDoctor),
            "view {view:?}"
        );
    }

    // Held keys move the selection and nothing else.
    assert_eq!(
        action_for_key(View::Doctor, repeat(KeyCode::Char('j'))),
        Some(Action::MoveDoctorSelection(1))
    );
    for code in [KeyCode::Tab, KeyCode::Enter, KeyCode::Esc] {
        assert_eq!(action_for_key(View::Doctor, repeat(code)), None, "{code:?}");
    }
}

#[test]
fn a_held_inventory_key_repeats_movement_but_not_navigation() {
    use skilled::input::action_for_key;

    assert_eq!(
        action_for_key(View::Inventory, repeat(KeyCode::Char('j'))),
        Some(Action::MoveInventorySelection(1))
    );
    for code in [
        KeyCode::Tab,
        KeyCode::Enter,
        KeyCode::Esc,
        KeyCode::Char('/'),
    ] {
        assert_eq!(
            action_for_key(View::Inventory, repeat(code)),
            None,
            "{code:?}"
        );
    }
}

/// The movement keys belong to whichever Inventory region has focus: they walk
/// the table's rows, and they move the detail region's window once the user has
/// drilled into it. The view alone cannot tell the two apart, so the pure
/// mapping still answers with the selection and the translation happens where
/// the application state is in hand.
#[test]
fn the_focused_inventory_region_decides_what_the_movement_keys_move() {
    use skilled::input::{action_for_app_key, action_for_key};

    let temporary = tempfile::tempdir().expect("temporary application directory");
    let skill = temporary.path().join("home/.claude/skills/portable");
    fs::create_dir_all(&skill).expect("create skill fixture");
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: portable\ndescription: Portable fixture\n---\n# Portable\n",
    )
    .expect("write skill fixture");
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    for _ in 0..7 {
        let update = app.update(Action::Continue);
        app.perform_effects(update.effects())
            .expect("setup effects");
    }
    assert_eq!(
        action_for_app_key(&app, key(KeyCode::Char('j'))),
        Some(Action::MoveInventorySelection(1))
    );

    app.update(Action::AdvanceInventoryPane);

    for (code, expected) in [
        (KeyCode::Char('j'), Action::ScrollDetail(1)),
        (KeyCode::Down, Action::ScrollDetail(1)),
        (KeyCode::Char('k'), Action::ScrollDetail(-1)),
        (KeyCode::Up, Action::ScrollDetail(-1)),
    ] {
        assert_eq!(
            action_for_app_key(&app, key(code)),
            Some(expected),
            "{code:?}"
        );
    }
    // Reading a long region is exactly where a key is held down.
    assert_eq!(
        action_for_app_key(&app, repeat(KeyCode::Char('j'))),
        Some(Action::ScrollDetail(1))
    );
    // Everything else the Inventory binds is unchanged by the region in focus.
    assert_eq!(
        action_for_app_key(&app, key(KeyCode::Tab)),
        Some(Action::MoveInventoryPane(1))
    );
    assert_eq!(
        action_for_key(View::Inventory, key(KeyCode::Char('j'))),
        Some(Action::MoveInventorySelection(1))
    );
}

#[test]
fn actions_remain_copyable_values() {
    fn assert_copy<T: Copy>() {}

    assert_copy::<Action>();
}

#[test]
fn question_mark_opens_help_in_every_implemented_top_level_view() {
    use skilled::input::action_for_key;

    for view in [
        View::Setup(SetupStep::Welcome),
        View::Inventory,
        View::Sources,
        View::Doctor,
        View::Settings,
    ] {
        assert_eq!(
            action_for_key(view, key(KeyCode::Char('?'))),
            Some(Action::OpenHelp),
            "view {view:?}"
        );
    }
}

#[test]
fn help_owns_input_until_escape_closes_it() {
    use skilled::input::action_for_app_key;

    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    app.update(Action::OpenHelp);

    assert_eq!(
        action_for_app_key(&app, key(KeyCode::Esc)),
        Some(Action::CloseHelp)
    );
    for blocked in [
        KeyCode::Char('q'),
        KeyCode::Char('?'),
        KeyCode::Enter,
        KeyCode::Char('2'),
    ] {
        assert_eq!(
            action_for_app_key(&app, key(blocked)),
            None,
            "key {blocked:?}"
        );
    }
    assert_eq!(
        action_for_app_key(
            &app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
        ),
        Some(Action::Quit)
    );
}

#[test]
fn escape_closes_help_before_the_underlying_settings_dialog() {
    use skilled::input::action_for_app_key;

    let temporary = tempfile::tempdir().expect("temporary application directory");
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    for _ in 0..7 {
        let update = app.update(Action::Continue);
        app.perform_effects(update.effects())
            .expect("setup effects");
    }
    app.update(Action::OpenSettings);
    app.update(Action::OpenHelp);

    let close_help = action_for_app_key(&app, key(KeyCode::Esc)).expect("close help action");
    app.update(close_help);

    assert_eq!(app.view(), View::Settings);
    assert_eq!(app.help_context(), None);
    assert_eq!(
        action_for_app_key(&app, key(KeyCode::Esc)),
        Some(Action::Back)
    );
}

#[test]
fn repeated_keys_only_move_the_agent_selection() {
    use skilled::input::action_for_key;

    assert_eq!(
        action_for_key(View::Setup(SetupStep::DetectAgents), repeat(KeyCode::Down)),
        Some(Action::MoveSelection(1))
    );
    assert_eq!(
        action_for_key(View::Setup(SetupStep::Welcome), repeat(KeyCode::Enter)),
        None
    );
    assert_eq!(
        action_for_key(
            View::Setup(SetupStep::DetectAgents),
            repeat(KeyCode::Char(' '))
        ),
        None
    );
    assert_eq!(action_for_key(View::Settings, repeat(KeyCode::Enter)), None);
    assert_eq!(
        action_for_key(View::Inventory, repeat(KeyCode::Char('?'))),
        None
    );
}

#[test]
fn repeated_sources_hierarchy_keys_do_not_skip_regions() {
    use skilled::input::action_for_key;

    for code in [KeyCode::Tab, KeyCode::BackTab, KeyCode::Enter, KeyCode::Esc] {
        assert_eq!(
            action_for_key(View::Sources, repeat(code)),
            None,
            "{code:?}"
        );
    }
}

#[test]
fn source_path_entry_treats_printable_keys_as_text_and_keeps_ctrl_c_as_quit() {
    use skilled::input::action_for_app_key;

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

    assert_eq!(
        action_for_app_key(&app, key(KeyCode::Char('q'))),
        Some(Action::AppendSourcePath('q'))
    );
    assert_eq!(
        action_for_app_key(&app, key(KeyCode::Char('?'))),
        Some(Action::AppendSourcePath('?'))
    );
    assert_eq!(
        action_for_app_key(&app, key(KeyCode::Enter)),
        Some(Action::SubmitSourcePath)
    );
    assert_eq!(
        action_for_app_key(
            &app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
        ),
        Some(Action::Quit)
    );
    assert_eq!(
        action_for_app_key(&app, repeat(KeyCode::Char('x'))),
        Some(Action::AppendSourcePath('x'))
    );
    assert_eq!(
        action_for_app_key(&app, repeat(KeyCode::Backspace)),
        Some(Action::DeleteSourcePathCharacter)
    );
    assert_eq!(action_for_app_key(&app, repeat(KeyCode::Enter)), None);
    assert_eq!(action_for_app_key(&app, repeat(KeyCode::Esc)), None);
}

#[test]
fn pending_catalog_confirmation_precedes_sources_region_navigation() {
    use skilled::input::action_for_app_key;

    let temporary = tempfile::tempdir().expect("temporary application directory");
    let repository = temporary.path().join("source");
    fs::create_dir_all(repository.join("skills/portable")).expect("create skill fixture");
    fs::write(
        repository.join("skills/portable/SKILL.md"),
        "---\nname: portable\ndescription: fixture\n---\n# Portable\n",
    )
    .expect("write skill fixture");
    initialize_repository(&repository);
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    for _ in 0..7 {
        let update = app.update(Action::Continue);
        app.perform_effects(update.effects())
            .expect("complete setup");
    }
    app.update(Action::OpenSources);
    app.update(Action::BeginAddSource);
    for character in repository.to_string_lossy().chars() {
        app.update(Action::AppendSourcePath(character));
    }
    let update = app.update(Action::SubmitSourcePath);
    app.perform_effects(update.effects())
        .expect("inspect source");
    assert!(app.pending_source().is_some());

    assert_eq!(
        action_for_app_key(&app, key(KeyCode::Enter)),
        Some(Action::ConfirmPendingSource)
    );
    assert_eq!(
        action_for_app_key(&app, key(KeyCode::Esc)),
        Some(Action::CancelSourceFlow)
    );
    assert_eq!(action_for_app_key(&app, key(KeyCode::Tab)), None);
    assert_eq!(action_for_app_key(&app, key(KeyCode::BackTab)), None);
    assert_eq!(action_for_app_key(&app, repeat(KeyCode::Enter)), None);
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn repeat(code: KeyCode) -> KeyEvent {
    KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Repeat)
}

/// `i` reaches the install flow only from the regions that stand on a variant.
///
/// The repositories pane stands on a source, and its variant selection is
/// whatever it was last left at — a key that acted there would install a row
/// the user is not looking at.
#[cfg(unix)]
#[test]
fn install_is_offered_only_where_a_variant_is_focused() {
    use skilled::input::action_for_app_key;

    let temporary = tempfile::tempdir().expect("temporary application directory");
    let repository = temporary.path().join("library");
    create_source_fixture(&repository);
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    for _ in 0..7 {
        let update = app.update(Action::Continue);
        app.perform_effects(update.effects())
            .expect("setup effects");
    }
    app.update(Action::OpenSources);

    // The repositories pane offers nothing to install.
    assert_eq!(action_for_app_key(&app, key(KeyCode::Char('i'))), None);
    assert!(!app.can_install_selection());

    app.update(Action::AdvanceSourcesPane);
    assert!(app.can_install_selection());
    assert_eq!(
        action_for_app_key(&app, key(KeyCode::Char('i'))),
        Some(Action::BeginInstall)
    );
    // A held key must not queue a second install of the same row.
    assert_eq!(action_for_app_key(&app, repeat(KeyCode::Char('i'))), None);

    // The Inventory has no variant to stand on, so `i` is not its key.
    let update = app.update(Action::OpenInventory);
    app.perform_effects(update.effects()).expect("scan");
    assert_eq!(action_for_app_key(&app, key(KeyCode::Char('i'))), None);
}

#[cfg(unix)]
#[test]
fn x_maps_only_to_the_owned_object_in_the_active_region() {
    use skilled::input::action_for_app_key;

    let temporary = tempfile::tempdir().expect("temporary application directory");
    let repository = temporary.path().join("library");
    create_source_fixture(&repository);
    let home = temporary.path().join("home");
    let mut app = SkilledApp::open(AppEnvironment::new(
        &home,
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    for _ in 0..7 {
        let update = app.update(Action::Continue);
        app.perform_effects(update.effects())
            .expect("setup effects");
    }

    app.update(Action::OpenSources);
    assert_eq!(
        action_for_app_key(&app, key(KeyCode::Char('x'))),
        Some(Action::BeginForgetSource)
    );
    app.update(Action::AdvanceSourcesPane);
    assert_eq!(action_for_app_key(&app, key(KeyCode::Char('x'))), None);

    for root in [".claude", ".agents", ".config/opencode"] {
        fs::create_dir_all(home.join(root)).expect("root parent");
    }
    let update = app.update(Action::BeginInstall);
    app.perform_effects(update.effects()).expect("plan install");
    app.note_detail_max_scroll(Some(0));
    let update = app.update(Action::ConfirmOperation);
    app.perform_effects(update.effects())
        .expect("apply install");
    app.update(Action::DismissOperation);
    let update = app.update(Action::OpenInventory);
    app.perform_effects(update.effects())
        .expect("scan inventory");

    assert_eq!(
        action_for_app_key(&app, key(KeyCode::Char('x'))),
        Some(Action::BeginUninstall)
    );
    assert_eq!(action_for_app_key(&app, repeat(KeyCode::Char('x'))), None);
    app.update(Action::BeginInventoryFilter);
    assert_eq!(
        action_for_app_key(&app, key(KeyCode::Char('x'))),
        Some(Action::AppendInventoryFilter('x'))
    );
}

/// The install preview owns the keyboard: only a confirmation, a dismissal, and
/// the one command no context may swallow get through.
#[cfg(unix)]
#[test]
fn the_install_prompt_owns_input_until_it_is_answered() {
    use skilled::input::action_for_app_key;

    let temporary = tempfile::tempdir().expect("temporary application directory");
    let repository = temporary.path().join("library");
    create_source_fixture(&repository);
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    for _ in 0..7 {
        let update = app.update(Action::Continue);
        app.perform_effects(update.effects())
            .expect("setup effects");
    }
    app.update(Action::OpenSources);
    app.update(Action::AdvanceSourcesPane);
    let update = app.update(Action::BeginInstall);
    app.perform_effects(update.effects()).expect("plan install");
    assert!(app.pending_install().is_some());

    assert_eq!(
        action_for_app_key(&app, key(KeyCode::Enter)),
        Some(Action::ConfirmOperation)
    );
    assert_eq!(
        action_for_app_key(&app, key(KeyCode::Esc)),
        Some(Action::DismissOperation)
    );
    for blocked in [
        KeyCode::Char('q'),
        KeyCode::Char('?'),
        KeyCode::Char('1'),
        KeyCode::Char('i'),
        KeyCode::Tab,
    ] {
        assert_eq!(
            action_for_app_key(&app, key(blocked)),
            None,
            "key {blocked:?}"
        );
    }
    assert_eq!(
        action_for_app_key(
            &app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
        ),
        Some(Action::Quit)
    );
}

#[cfg(unix)]
fn create_source_fixture(repository: &Path) {
    fs::create_dir_all(repository.join("skills/portable")).expect("create source fixture");
    fs::write(
        repository.join("skills/portable/SKILL.md"),
        "---\nname: portable\ndescription: Portable fixture\n---\n# Portable\n",
    )
    .expect("write source fixture");
    initialize_repository(repository);
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
    assert!(output.status.success());
}

/// Filter input needs a real installed skill to narrow.
///
/// Managed installations are symbolic links, so the fixture is Unix-only.
#[cfg(unix)]
mod installed {
    use super::*;

    #[test]
    fn the_inventory_filter_takes_printable_keys_as_text_and_keeps_ctrl_c_as_quit() {
        use skilled::input::action_for_app_key;

        let temporary = tempfile::tempdir().expect("temporary application directory");
        let repository = temporary.path().join("library");
        let mut app = SkilledApp::open(AppEnvironment::new(
            temporary.path().join("home"),
            temporary.path().join("data"),
            "",
        ))
        .expect("open application");
        for _ in 0..7 {
            let update = app.update(Action::Continue);
            app.perform_effects(update.effects())
                .expect("setup effects");
        }
        create_source_fixture(&repository);
        let preview = app.preview_source(&repository).expect("preview source");
        app.confirm_source(preview).expect("register source");
        install_link(
            &temporary.path().join("home"),
            "portable",
            &repository.join("skills/portable"),
        );
        let update = app.update(Action::OpenSources);
        app.perform_effects(update.effects()).expect("effects");
        let update = app.update(Action::OpenInventory);
        app.perform_effects(update.effects()).expect("scan");
        app.update(Action::BeginInventoryFilter);
        assert!(app.inventory_filter_active());

        // Every printable key is text, including the ones that are routes outside
        // the filter, so a query can name a source or a destination digit.
        for (code, expected) in [
            (KeyCode::Char('2'), Action::AppendInventoryFilter('2')),
            (KeyCode::Char('s'), Action::AppendInventoryFilter('s')),
            (KeyCode::Char('?'), Action::AppendInventoryFilter('?')),
            (KeyCode::Char('/'), Action::AppendInventoryFilter('/')),
            (KeyCode::Backspace, Action::DeleteInventoryFilterCharacter),
            (KeyCode::Enter, Action::SubmitInventoryFilter),
            (KeyCode::Esc, Action::Back),
        ] {
            assert_eq!(
                action_for_app_key(&app, key(code)),
                Some(expected),
                "{code:?}"
            );
        }
        assert_eq!(
            action_for_app_key(
                &app,
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
            ),
            Some(Action::Quit)
        );
        // Held keys type and delete; nothing else repeats.
        assert_eq!(
            action_for_app_key(&app, repeat(KeyCode::Char('a'))),
            Some(Action::AppendInventoryFilter('a'))
        );
        assert_eq!(action_for_app_key(&app, repeat(KeyCode::Enter)), None);
    }
    fn install_link(home: &Path, name: &str, target: &Path) {
        let root = home.join(".claude/skills");
        fs::create_dir_all(&root).expect("create agent skill root");
        std::os::unix::fs::symlink(target, root.join(name)).expect("install symbolic link");
    }
}
