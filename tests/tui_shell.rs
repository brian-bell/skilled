use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Terminal,
    backend::TestBackend,
    buffer::Buffer,
    style::{Color, Modifier, Style},
};
use std::{fs, path::Path, process::Command};

use skilled::{Action, AgentKind, AppEnvironment, SkilledApp};

#[test]
fn the_shell_frames_inventory_with_product_navigation_and_key_hints() {
    let harness = Harness::new();
    let app = harness.completed_setup();

    let screen = buffer(&app, 80, 24);

    assert!(
        row_text(&screen, 0).contains("skilled"),
        "{}",
        text(&screen)
    );
    assert!(
        row_text(&screen, 1).contains("Inventory"),
        "{}",
        text(&screen)
    );
    assert!(
        row_text(&screen, 1).contains("2 Sources"),
        "{}",
        text(&screen)
    );
    assert!(row_text(&screen, 23).contains("Quit"), "{}", text(&screen));
}

#[test]
fn the_title_bar_keeps_its_own_colours_beside_the_session_status() {
    let harness = Harness::new();
    let screen = buffer(&harness.completed_setup(), 80, 24);

    // The status is right-aligned on the same row. Rendering it across the
    // whole row would repaint the product mark and wordmark; these assertions
    // are the regression guard for that.
    assert_eq!(
        style_in_row(&screen, 0, "◆").fg,
        Some(Color::Rgb(0x8b, 0xd4, 0x9c))
    );
    let wordmark = style_in_row(&screen, 0, "skilled");
    assert_eq!(wordmark.fg, Some(Color::Rgb(0xf2, 0xf6, 0xfa)));
    assert!(wordmark.add_modifier.contains(Modifier::BOLD));
    assert_eq!(
        style_in_row(&screen, 0, "●").fg,
        Some(Color::Rgb(0x8b, 0xd4, 0x9c))
    );
}

#[test]
fn an_open_dialog_takes_the_navigation_row_with_it() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    create_source_fixture(&repository);

    // Settings: 1 and 2 are unmapped.
    let mut settings = harness.completed_setup();
    settings.update(Action::OpenSettings);
    let row = row_text(&buffer(&settings, 80, 24), 1);
    assert!(row.contains("Settings"), "{row}");
    assert!(row.contains("navigation is locked"), "{row}");
    assert!(!row.contains("Inventory"), "{row}");

    // Add source: 1 and 2 type characters into the path.
    let mut adding = harness.completed_setup();
    adding.update(Action::OpenSources);
    adding.update(Action::BeginAddSource);
    let row = row_text(&buffer(&adding, 80, 24), 1);
    assert!(row.contains("Add source"), "{row}");
    assert!(!row.contains("Sources  "), "{row}");

    // Confirm catalogs from Sources: 1, 2 and 3 toggle agent compatibility.
    let mut confirming = harness.completed_setup();
    confirming.update(Action::OpenSources);
    confirming.update(Action::BeginAddSource);
    for character in repository.to_string_lossy().chars() {
        confirming.update(Action::AppendSourcePath(character));
    }
    let update = confirming.update(Action::SubmitSourcePath);
    confirming
        .perform_effects(update.effects())
        .expect("inspect source");
    assert!(confirming.pending_source().is_some());
    for (code, expected) in [
        (KeyCode::Enter, Action::ConfirmPendingSource),
        (KeyCode::Esc, Action::CancelSourceFlow),
        (KeyCode::Up, Action::MoveCatalogSelection(-1)),
        (KeyCode::Char('k'), Action::MoveCatalogSelection(-1)),
        (KeyCode::Down, Action::MoveCatalogSelection(1)),
        (KeyCode::Char('j'), Action::MoveCatalogSelection(1)),
        (KeyCode::Char(' '), Action::ToggleCatalogIncluded),
        (KeyCode::Char('c'), Action::ToggleCatalogClassification),
        (
            KeyCode::Char('1'),
            Action::ToggleCatalogCompatibility(AgentKind::ClaudeCode),
        ),
        (
            KeyCode::Char('2'),
            Action::ToggleCatalogCompatibility(AgentKind::Codex),
        ),
        (
            KeyCode::Char('3'),
            Action::ToggleCatalogCompatibility(AgentKind::OpenCode),
        ),
    ] {
        assert_eq!(
            skilled::input::action_for_app_key(
                &confirming,
                KeyEvent::new(code, KeyModifiers::NONE),
            ),
            Some(expected),
            "catalog confirmation key {code:?}"
        );
    }
    assert_eq!(
        skilled::input::action_for_app_key(
            &confirming,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        ),
        None
    );
    confirming.update(Action::OpenHelp);
    assert_eq!(confirming.help_context(), None);
    let row = row_text(&buffer(&confirming, 80, 24), 1);
    assert!(row.contains("Confirm catalogs"), "{row}");
    assert!(!row.contains("Sources  "), "{row}");
}

#[test]
fn setup_says_navigation_waits_for_setup_not_for_a_dialog() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    create_source_fixture(&repository);
    let mut app = harness.first_run();
    // Reach "Discover sources" and register a source from inside setup.
    for _ in 0..3 {
        let update = app.update(Action::Continue);
        app.perform_effects(update.effects())
            .expect("setup effects");
    }
    app.update(Action::BeginAddSource);

    // The dialog is on screen, but navigation is waiting on setup, not on it.
    let row = row_text(&buffer(&app, 80, 24), 1);
    assert!(row.contains("Add source"), "{row}");
    assert!(row.contains("locked during setup"), "{row}");
    assert!(!row.contains("this dialog"), "{row}");

    for character in repository.to_string_lossy().chars() {
        app.update(Action::AppendSourcePath(character));
    }
    let update = app.update(Action::SubmitSourcePath);
    app.perform_effects(update.effects())
        .expect("inspect source");
    assert!(app.pending_source().is_some());

    // A pending source inside setup is shown inline, so the row must not claim
    // a dialog is open.
    let screen = buffer(&app, 80, 24);
    let row = row_text(&screen, 1);
    assert!(row.contains("Setup · Confirm catalogs"), "{row}");
    assert!(row.contains("locked during setup"), "{row}");
    assert!(!row.contains("this dialog"), "{row}");
}

#[test]
fn the_empty_state_styles_its_glyph_headline_and_body_distinctly() {
    let harness = Harness::new();
    let screen = buffer(&harness.completed_setup(), 80, 24);

    assert_eq!(
        style_at(&screen, "⌕").fg,
        Some(Color::Rgb(0x53, 0x61, 0x71))
    );
    let headline = style_at(&screen, "Installation roots have not been");
    assert_eq!(headline.fg, Some(Color::Rgb(0xd7, 0xde, 0xe7)));
    assert!(headline.add_modifier.contains(Modifier::BOLD));

    let body = style_at(&screen, "Skilled has not looked");
    assert_eq!(body.fg, Some(Color::Rgb(0x84, 0x91, 0xa1)));
    assert!(!body.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn surfaces_are_painted_where_the_design_calls_for_them() {
    // The background layering is what keeps chrome and workspace from
    // disagreeing, and what makes a badge legible inside a dialog. Without
    // these assertions the layering can regress silently.
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    create_source_fixture(&repository);
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    app.update(Action::OpenSources);
    let screen = buffer(&app, 120, 40);

    const TERMINAL: Color = Color::Rgb(0x0b, 0x0f, 0x14);
    const SURFACE: Color = Color::Rgb(0x0f, 0x15, 0x1d);
    const SURFACE_2: Color = Color::Rgb(0x12, 0x1a, 0x24);
    const SURFACE_3: Color = Color::Rgb(0x17, 0x21, 0x2c);

    // One canvas under everything, chrome and workspace alike.
    assert_eq!(style_in_row(&screen, 0, "skilled").bg, Some(TERMINAL));
    assert_eq!(
        style_in_row(&screen, row_containing(&screen, "Repositories"), "┌").bg,
        Some(TERMINAL)
    );

    // The navigation strip is its own band, with the active tab lifted.
    // Sources is the active tab here, so Inventory is the inactive probe.
    assert_eq!(style_in_row(&screen, 1, "1 Inventory").bg, Some(SURFACE));
    assert_eq!(style_in_row(&screen, 1, "▌Sources").bg, Some(SURFACE_2));

    // The focused row is tinted across the pane, not just behind its label.
    let focused = row_containing(&screen, "▌ source");
    assert_eq!(style_in_row(&screen, focused, "source").bg, Some(SURFACE_3));
    // The band must reach the end of the pane's text area, which is what
    // distinguishes it from a label-length smear. The pane keeps one column of
    // padding inside its border, so the last text cell is two columns in.
    let row = row_text(&screen, focused);
    let right_border = row
        .char_indices()
        .filter(|(_, character)| *character == '│')
        .nth(1)
        .map(|(index, _)| u16::try_from(row[..index].chars().count()).expect("column"))
        .expect("Repositories pane right border");
    assert_eq!(
        screen[(right_border - 2, focused)].style().bg,
        Some(SURFACE_3),
        "the tint should reach the end of the pane, not stop at the label"
    );

    // A badge inside a dialog must not stamp the canvas colour over the
    // dialog's own surface.
    let mut dialog = harness.completed_setup();
    dialog.update(Action::OpenSources);
    dialog.update(Action::BeginAddSource);
    for character in repository.to_string_lossy().chars() {
        dialog.update(Action::AppendSourcePath(character));
    }
    let update = dialog.update(Action::SubmitSourcePath);
    dialog
        .perform_effects(update.effects())
        .expect("inspect source");
    let screen = buffer(&dialog, 120, 40);
    let branch = row_containing(&screen, "Branch:");
    assert_eq!(style_in_row(&screen, branch, "Branch:").bg, Some(SURFACE));
    assert_eq!(
        style_in_row(&screen, branch, "✓ clean").bg,
        Some(SURFACE),
        "a badge should inherit the dialog surface, not repaint it"
    );
}

#[test]
fn the_setup_summary_counts_only_what_exists() {
    let harness = Harness::new();
    let mut app = harness.first_run();
    for _ in 0..6 {
        let update = app.update(Action::Continue);
        app.perform_effects(update.effects())
            .expect("setup effects");
    }

    let screen = text(&buffer(&app, 80, 24));

    assert!(screen.contains("Setup is ready to finish."), "{screen}");
    assert!(screen.contains("Sources: 0"), "{screen}");
    // Nothing scans installations and nothing produces findings yet.
    assert!(!screen.contains("Installations:"), "{screen}");
    assert!(!screen.contains("Doctor findings"), "{screen}");
}

#[test]
fn colours_are_defined_only_by_the_theme() {
    // The theme is the single place a palette decision can be made. Without
    // this guard the rule silently decays the next time a screen needs a hue.
    let mut pending = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("read source directory") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs")
                || path.file_name().is_some_and(|name| name == "theme.rs")
            {
                continue;
            }
            let contents = fs::read_to_string(&path).expect("read source file");
            assert!(
                !contents.contains("Color::"),
                "{} names a colour directly; add a semantic token to theme.rs instead",
                path.display()
            );
        }
    }
}

#[test]
fn navigation_separates_active_reachable_and_unavailable_destinations() {
    let harness = Harness::new();
    let app = harness.completed_setup();

    let screen = buffer(&app, 80, 24);
    let navigation = row_text(&screen, 1);

    // Focus is carried by a marker and emphasis, not by colour alone.
    assert!(navigation.contains("▌Inventory"), "{navigation}");
    assert!(navigation.contains(" 2 Sources"), "{navigation}");
    assert!(!navigation.contains("▌2 Sources"), "{navigation}");
    // Pressing 1 on Inventory does nothing, so no shortcut is offered for it.
    assert!(!navigation.contains("1 Inventory"), "{navigation}");

    let active = style_at(&screen, "Inventory");
    assert_eq!(active.fg, Some(Color::Rgb(0xf2, 0xf6, 0xfa)));
    assert!(active.add_modifier.contains(Modifier::BOLD));
    assert!(active.add_modifier.contains(Modifier::UNDERLINED));

    let reachable = style_at(&screen, "2 Sources");
    assert_eq!(reachable.fg, Some(Color::Rgb(0x84, 0x91, 0xa1)));
    assert!(!reachable.add_modifier.contains(Modifier::BOLD));

    // Views without an implementation are visibly unavailable rather than
    // absent, and offer no shortcut, because 3 and 4 are unmapped everywhere.
    assert!(navigation.contains("Updates (soon)"), "{navigation}");
    assert!(navigation.contains("Doctor (soon)"), "{navigation}");
    assert!(!navigation.contains("3 Updates"), "{navigation}");
    assert!(!navigation.contains("4 Doctor"), "{navigation}");

    // One de-emphasis mechanism, plus the word "(soon)" for anyone who cannot
    // perceive it.
    let unavailable = style_at(&screen, "Updates");
    assert_eq!(unavailable.fg, Some(Color::Rgb(0x53, 0x61, 0x71)));
    assert!(!unavailable.add_modifier.contains(Modifier::DIM));
}

#[test]
fn navigation_does_not_offer_routes_that_setup_blocks() {
    let harness = Harness::new();
    let mut app = harness.first_run();
    app.update(Action::Continue);

    let navigation = row_text(&buffer(&app, 80, 24), 1);

    // Keys 1 and 2 do nothing during setup, so no tab may look reachable.
    assert!(!navigation.contains("Inventory"), "{navigation}");
    assert!(!navigation.contains("Sources"), "{navigation}");
    // The row still carries the persistent frame's sense of place.
    assert!(navigation.contains("Setup · Detect agents"), "{navigation}");
    assert!(
        navigation.contains("navigation is locked during setup"),
        "{navigation}"
    );
}

#[test]
fn session_status_reports_only_what_the_application_knows() {
    let harness = Harness::new();

    let during_setup = row_text(&buffer(&harness.first_run(), 80, 24), 0);
    assert!(during_setup.contains("setup in progress"), "{during_setup}");

    let after_setup = row_text(&buffer(&harness.completed_setup(), 80, 24), 0);
    assert!(
        after_setup.contains("ready · 0 sources registered"),
        "{after_setup}"
    );
    // Nothing is scanned yet, so no scan, finding, or timestamp may be claimed.
    assert!(!after_setup.contains("scan"), "{after_setup}");
    assert!(!after_setup.contains("ago"), "{after_setup}");
}

#[test]
fn navigation_follows_the_active_view() {
    let harness = Harness::new();
    let mut app = harness.completed_setup();
    app.update(Action::OpenSources);

    let navigation = row_text(&buffer(&app, 80, 24), 1);

    // Sources is active so it drops its own digit, and Inventory gains one.
    assert!(navigation.contains("▌Sources"), "{navigation}");
    assert!(navigation.contains("1 Inventory"), "{navigation}");
    assert!(!navigation.contains("2 Sources"), "{navigation}");
}

#[test]
fn key_hints_advertise_only_commands_this_release_implements() {
    let harness = Harness::new();
    let app = harness.completed_setup();

    let hints = row_text(&buffer(&app, 80, 24), 23);

    assert!(hints.contains("2 Sources"), "{hints}");
    assert!(hints.contains("s Settings"), "{hints}");
    assert!(hints.contains("? Help"), "{hints}");
    assert!(hints.contains("q Quit"), "{hints}");
    for absent in [
        "Install",
        "Uninstall",
        "Forget",
        "Repair",
        "Update",
        "Filter",
    ] {
        assert!(!hints.contains(absent), "{absent} in {hints}");
    }
}

#[test]
fn contextual_help_names_its_context_and_exit() {
    let harness = Harness::new();
    let mut app = harness.first_run();
    app.update(Action::OpenHelp);

    let rendered = text(&buffer(&app, 80, 24));

    assert!(rendered.contains("Keyboard reference"), "{rendered}");
    assert!(rendered.contains("Setup · Welcome and scope"), "{rendered}");
    assert!(rendered.contains("Esc Close"), "{rendered}");
}

#[test]
fn contextual_help_has_a_semantic_modal_footer() {
    let harness = Harness::new();
    let mut app = harness.completed_setup();
    app.update(Action::OpenHelp);

    let screen = buffer(&app, 80, 24);
    let rendered = text(&screen);

    assert!(
        row_text(&screen, 1).contains("Keyboard reference"),
        "{rendered}"
    );
    assert!(
        row_text(&screen, 1).contains("navigation is locked"),
        "{rendered}"
    );
    assert!(rendered.contains("Commands for Inventory"), "{rendered}");
    assert!(rendered.matches("Esc Close").count() >= 2, "{rendered}");
    assert!(rendered.contains("┌ Keyboard reference"), "{rendered}");
    assert!(rendered.contains("└"), "{rendered}");
}

#[test]
fn inventory_help_lists_only_implemented_inventory_commands() {
    let harness = Harness::new();
    let mut app = harness.completed_setup();
    app.update(Action::OpenHelp);

    let rendered = text(&buffer(&app, 80, 24));

    for command in ["2 Sources", "s Settings", "? Help", "q Quit"] {
        assert!(
            rendered.contains(command),
            "missing {command:?} in\n{rendered}"
        );
    }
    for unavailable in [
        "Install",
        "Update",
        "Repair",
        "Uninstall",
        "Forget",
        "Filter",
        "Doctor",
    ] {
        assert!(
            !rendered.contains(unavailable),
            "unexpected {unavailable:?} in\n{rendered}"
        );
    }
}

#[test]
fn setup_help_changes_with_the_active_step() {
    let harness = Harness::new();
    let mut app = harness.first_run();
    app.update(Action::OpenHelp);

    let welcome = text(&buffer(&app, 80, 24));
    assert!(welcome.contains("Enter Continue"), "{welcome}");
    assert!(!welcome.contains("Esc Back"), "{welcome}");
    assert!(!welcome.contains("j/k Move"), "{welcome}");
    assert!(!welcome.contains("Space Toggle"), "{welcome}");

    app.update(Action::CloseHelp);
    app.update(Action::Continue);
    app.update(Action::OpenHelp);

    let detect_agents = text(&buffer(&app, 80, 24));
    assert!(detect_agents.contains("j/k Move"), "{detect_agents}");
    assert!(detect_agents.contains("Space Toggle"), "{detect_agents}");
    assert!(detect_agents.contains("Esc Back"), "{detect_agents}");
}

#[test]
fn sources_and_settings_help_match_their_active_bindings() {
    let harness = Harness::new();
    let mut sources = harness.completed_setup();
    sources.update(Action::OpenSources);
    sources.update(Action::OpenHelp);

    let sources_help = text(&buffer(&sources, 120, 40));
    for command in [
        "Tab / Shift-Tab Pane",
        "j/k Move",
        "a Add source",
        "1 Inventory",
        "Esc Back",
        "? Help",
        "q Quit",
    ] {
        assert!(
            sources_help.contains(command),
            "missing {command:?} in\n{sources_help}"
        );
    }

    let mut settings = harness.completed_setup();
    settings.update(Action::OpenSettings);
    settings.update(Action::OpenHelp);

    let settings_help = text(&buffer(&settings, 80, 24));
    for command in ["Enter Rerun setup", "Esc Close Settings", "? Help"] {
        assert!(
            settings_help.contains(command),
            "missing {command:?} in\n{settings_help}"
        );
    }
    assert!(!settings_help.contains("q Quit"), "{settings_help}");
}

#[test]
fn wide_help_balances_commands_across_two_columns() {
    let harness = Harness::new();
    let mut app = harness.completed_setup();
    app.update(Action::OpenSources);
    app.update(Action::OpenHelp);

    let screen = buffer(&app, 120, 40);
    let first_command_row = row_containing(&screen, "Tab / Shift-Tab");
    let row = row_text(&screen, first_command_row);

    assert!(row.contains("Esc Back"), "{row}\n{}", text(&screen));
}

#[test]
fn complete_key_hints_render_at_compact_and_wide_sizes() {
    let harness = Harness::new();
    let mut setup = harness.first_run();
    setup.update(Action::Continue);
    let inventory = harness.completed_setup();
    let mut sources = harness.completed_setup();
    sources.update(Action::OpenSources);
    let mut settings = harness.completed_setup();
    settings.update(Action::OpenSettings);

    let contexts: [(&SkilledApp, &str); 4] = [
        (
            &setup,
            " j/k Move   Space Toggle   Enter Continue   Esc Back   ? Help   q Quit",
        ),
        (&inventory, " 2 Sources   s Settings   ? Help   q Quit"),
        (
            &sources,
            " Tab Pane   j/k Move   a Add source   1 Inventory   ? Help   q Quit",
        ),
        (&settings, " Enter Rerun setup   ? Help   Esc Close"),
    ];

    for (width, height) in [(80, 24), (120, 40)] {
        for (app, expected) in contexts {
            let screen = buffer(app, width, height);
            let footer = row_text(&screen, height - 1);
            assert_eq!(footer, expected, "unexpected hints at {width}x{height}");
        }
    }
}

#[test]
fn every_help_context_omits_unavailable_commands_and_owns_the_footer() {
    let harness = Harness::new();
    let mut app = harness.first_run();
    let assert_truthful_help = |app: &SkilledApp| {
        let screen = buffer(app, 80, 24);
        let rendered = text(&screen);
        for unavailable in [
            "Install",
            "Update",
            "Repair",
            "Uninstall",
            "Forget",
            "Filter",
            "Updates",
            "Doctor",
        ] {
            assert!(
                !rendered.contains(unavailable),
                "unexpected {unavailable:?} in\n{rendered}"
            );
        }
        assert_eq!(
            row_text(&screen, 23),
            " Esc Close   Ctrl-C Quit",
            "help must hide q and every underlying command"
        );
    };

    for _ in 0..7 {
        app.update(Action::OpenHelp);
        assert_truthful_help(&app);
        app.update(Action::CloseHelp);
        app.update(Action::Continue);
    }

    app.update(Action::OpenHelp);
    assert_truthful_help(&app);
    app.update(Action::CloseHelp);
    app.update(Action::OpenSources);
    app.update(Action::OpenHelp);
    assert_truthful_help(&app);
    app.update(Action::CloseHelp);
    app.update(Action::Back);
    app.update(Action::OpenSettings);
    app.update(Action::OpenHelp);
    assert_truthful_help(&app);
}

#[test]
fn key_caps_are_emphasised_apart_from_their_labels() {
    let harness = Harness::new();
    let app = harness.completed_setup();
    let screen = buffer(&app, 80, 24);

    let cap = style_at(&screen, "q Quit");
    assert!(cap.add_modifier.contains(Modifier::BOLD));
    assert_eq!(cap.fg, Some(Color::Rgb(0xf2, 0xf6, 0xfa)));

    let label = style_at(&screen, "Quit");
    assert!(!label.add_modifier.contains(Modifier::BOLD));
    assert_eq!(label.fg, Some(Color::Rgb(0x84, 0x91, 0xa1)));
}

#[test]
fn empty_inventory_states_what_is_known_without_inventing_data() {
    let harness = Harness::new();
    let app = harness.completed_setup();

    let screen = text(&buffer(&app, 80, 24));

    assert!(screen.contains("Global inventory"), "{screen}");
    assert!(screen.contains("not scanned"), "{screen}");
    // The headline reports the absence of a scan, not the result of one.
    assert!(
        screen.contains("Installation roots have not been scanned"),
        "{screen}"
    );
    assert!(!screen.contains("No installed skills found"), "{screen}");
    // A zero count would itself be a scan result, and no scan has run.
    assert!(!screen.contains("0 skills"), "{screen}");
    assert!(
        screen.contains("Skilled has not looked at any installation root yet"),
        "{screen}"
    );

    // Doctor, updates, and installation do not exist yet, so the empty state
    // may not report their results or offer their actions.
    for invented in [
        "Doctor findings",
        "findings:",
        "Uninstall",
        "Repair",
        "Update available",
        "healthy",
    ] {
        assert!(!screen.contains(invented), "{invented} in\n{screen}");
    }
    // No per-skill status either: there are no skills to carry one.
    for glyph in ["✓", "×", "!"] {
        assert!(!screen.contains(glyph), "{glyph} in\n{screen}");
    }
}

#[test]
fn wide_terminals_gain_a_detail_region_and_compact_ones_do_not() {
    let harness = Harness::new();
    let app = harness.completed_setup();

    let wide = text(&buffer(&app, 120, 40));
    assert!(wide.contains("Details"), "{wide}");
    assert!(wide.contains("Nothing to show"), "{wide}");
    assert!(wide.contains("Identity, provenance, and"), "{wide}");
    // Nothing is selectable yet, so the region must not imply that it is.
    assert!(!wide.contains("Select a skill"), "{wide}");
    // Both regions are present, so the primary empty state still reads.
    assert!(
        wide.contains("Installation roots have not been scanned"),
        "{wide}"
    );

    let compact = text(&buffer(&app, 80, 24));
    assert!(!compact.contains("Nothing to show"), "{compact}");
    assert!(
        compact.contains("Installation roots have not been scanned"),
        "{compact}"
    );
}

#[test]
fn source_status_pairs_every_colour_with_a_glyph_and_a_word() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    create_source_fixture(&repository);
    fs::create_dir_all(repository.join("skills/broken")).expect("create invalid candidate");
    fs::write(repository.join("skills/broken/skill.md"), "wrong filename")
        .expect("write invalid candidate");
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    app.update(Action::OpenSources);

    let screen = buffer(&app, 120, 40);
    let rendered = text(&screen);

    // A committed fixture with an untracked directory is dirty, and the state
    // is spelled out rather than implied by colour.
    assert!(rendered.contains("! dirty"), "{rendered}");
    assert_eq!(
        style_at(&screen, "! dirty").fg,
        Some(Color::Rgb(0xe6, 0xbd, 0x6a))
    );

    assert!(rendered.contains("✓ portable"), "{rendered}");
    assert_eq!(
        style_at(&screen, "✓ portable").fg,
        Some(Color::Rgb(0x8b, 0xd4, 0x9c))
    );

    assert!(rendered.contains("× broken"), "{rendered}");
    assert_eq!(
        style_at(&screen, "× broken").fg,
        Some(Color::Rgb(0xee, 0x6b, 0x73))
    );
}

#[test]
fn dialogs_share_a_modal_frame_that_names_itself_and_its_exit() {
    let harness = Harness::new();
    let mut app = harness.completed_setup();
    app.update(Action::OpenSettings);

    let screen = buffer(&app, 80, 24);
    let rendered = text(&screen);

    // The frame is drawn, not merely tinted, so modality survives without colour.
    let border = row_containing(&screen, "┌ Settings");
    assert!(rendered.contains("┘"), "{rendered}");
    assert_eq!(
        style_in_row(&screen, border, "┌").fg,
        Some(Color::Rgb(0x43, 0x52, 0x64))
    );

    // The header names the dialog and its scope; the body states the way out.
    assert!(rendered.contains("global scope"), "{rendered}");
    assert!(rendered.contains("Esc closes"), "{rendered}");

    let title = style_in_row(&screen, border, "Settings");
    assert!(title.add_modifier.contains(Modifier::BOLD));
    assert_eq!(title.fg, Some(Color::Rgb(0xf2, 0xf6, 0xfa)));
}

#[test]
fn the_source_dialogs_use_the_same_frame_as_settings() {
    let harness = Harness::new();
    let mut app = harness.completed_setup();
    app.update(Action::OpenSources);
    app.update(Action::BeginAddSource);

    let screen = buffer(&app, 80, 24);
    let rendered = text(&screen);

    assert!(rendered.contains("local Git checkout"), "{rendered}");
    let border = row_containing(&screen, "┌ Add source");
    assert_eq!(
        style_in_row(&screen, border, "┌").fg,
        Some(Color::Rgb(0x43, 0x52, 0x64))
    );
}

#[test]
fn the_focused_row_is_marked_and_emphasised_not_merely_tinted() {
    let harness = Harness::new();
    let first = harness.directory.path().join("first");
    let second = harness.directory.path().join("second");
    create_source_fixture(&first);
    create_source_fixture(&second);
    let mut app = harness.completed_setup();
    for repository in [&first, &second] {
        let preview = app.preview_source(repository).expect("preview source");
        app.confirm_source(preview).expect("register source");
    }
    app.update(Action::OpenSources);

    let screen = buffer(&app, 120, 40);
    // Registration focuses the newest source. Rows are matched by their whole
    // prefix so the repository list is not confused with the variants pane.
    let focused = row_starting_with(&screen, "│ ▌ second");
    let unfocused = row_starting_with(&screen, "│   first");

    assert!(
        style_in_row(&screen, focused, "second")
            .add_modifier
            .contains(Modifier::BOLD),
        "{}",
        text(&screen)
    );
    assert!(
        !style_in_row(&screen, unfocused, "first")
            .add_modifier
            .contains(Modifier::BOLD),
        "{}",
        text(&screen)
    );
    assert_eq!(
        style_in_row(&screen, focused, "▌").fg,
        Some(Color::Rgb(0x73, 0xd7, 0xee))
    );
}

#[test]
fn the_size_notice_survives_the_shell() {
    let harness = Harness::new();
    let screen = text(&buffer(&harness.completed_setup(), 60, 14));

    assert!(screen.contains("Terminal too small"), "{screen}");
    assert!(screen.contains("80×24"), "{screen}");
    // The frame must not be half-drawn into a viewport that cannot hold it.
    assert!(!screen.contains("Inventory"), "{screen}");
}

#[test]
fn control_characters_stay_escaped_under_the_shell() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    fs::create_dir_all(repository.join("skills/portable")).expect("create skill directory");
    fs::write(
        repository.join("skills/portable/SKILL.md"),
        "---\nname: portable\ndescription: \"fixture\\u001b]8;;https://example.test\"\n---\n# Fixture\n",
    )
    .expect("write skill");
    git(&repository, &["init", "-b", "main"]);
    git(&repository, &["config", "user.name", "Skilled Test"]);
    git(
        &repository,
        &["config", "user.email", "skilled@example.test"],
    );
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "fixture"]);
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    app.update(Action::OpenSources);

    let screen = text(&buffer(&app, 120, 40));

    assert!(
        !screen.contains('\u{1b}'),
        "escape sequence reached the buffer"
    );
    assert!(
        screen.contains("\\u{1b}") || screen.contains("\\x1b"),
        "{screen}"
    );
}

struct Harness {
    directory: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        Self {
            directory: tempfile::tempdir().expect("temporary application directory"),
        }
    }

    fn environment(&self) -> AppEnvironment {
        AppEnvironment::new(
            self.directory.path().join("home"),
            self.directory.path().join("data"),
            "",
        )
    }

    fn first_run(&self) -> SkilledApp {
        SkilledApp::open(self.environment()).expect("open application")
    }

    fn completed_setup(&self) -> SkilledApp {
        let mut app = self.first_run();
        for _ in 0..7 {
            let update = app.update(Action::Continue);
            app.perform_effects(update.effects())
                .expect("perform setup effects");
        }
        app
    }
}

fn buffer(app: &SkilledApp, width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("create test terminal");
    terminal
        .draw(|frame| skilled::tui::render(frame, app))
        .expect("render frame");
    terminal.backend().buffer().clone()
}

fn row_text(buffer: &Buffer, y: u16) -> String {
    let area = buffer.area;
    let mut line = String::new();
    for x in area.x..area.x + area.width {
        line.push_str(buffer[(x, y)].symbol());
    }
    line.trim_end().to_owned()
}

fn text(buffer: &Buffer) -> String {
    let area = buffer.area;
    (area.y..area.y + area.height)
        .map(|y| row_text(buffer, y))
        .collect::<Vec<_>>()
        .join("\n")
}

fn row_starting_with(buffer: &Buffer, prefix: &str) -> u16 {
    let area = buffer.area;
    (area.y..area.y + area.height)
        .find(|y| row_text(buffer, *y).starts_with(prefix))
        .unwrap_or_else(|| panic!("no row starts with {prefix:?} in\n{}", text(buffer)))
}

fn row_containing(buffer: &Buffer, needle: &str) -> u16 {
    let area = buffer.area;
    (area.y..area.y + area.height)
        .find(|y| row_text(buffer, *y).contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} not found in\n{}", text(buffer)))
}

fn style_in_row(buffer: &Buffer, y: u16, needle: &str) -> Style {
    let row = row_text(buffer, y);
    let byte_index = row
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not found in row {y}: {row:?}"));
    let column = row[..byte_index].chars().count() as u16;
    buffer[(buffer.area.x + column, y)].style()
}

fn style_at(buffer: &Buffer, needle: &str) -> Style {
    let area = buffer.area;
    for y in area.y..area.y + area.height {
        let row = row_text(buffer, y);
        if let Some(byte_index) = row.find(needle) {
            let column = row[..byte_index].chars().count() as u16;
            return buffer[(area.x + column, y)].style();
        }
    }
    panic!("{needle:?} not found in\n{}", text(buffer));
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
