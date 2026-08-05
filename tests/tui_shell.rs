use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Terminal,
    backend::TestBackend,
    buffer::Buffer,
    style::{Color, Modifier, Style},
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use skilled::{Action, AgentKind, AppEnvironment, SkilledApp};

#[test]
fn setup_uses_the_shared_dialog_and_seven_segment_progress() {
    let harness = Harness::new();
    let screen = buffer(&harness.first_run(), 80, 24);
    let rendered = text(&screen);

    assert!(rendered.contains("┌ First-run setup"), "{rendered}");
    assert!(rendered.contains("global skills only"), "{rendered}");
    assert!(rendered.contains("STEP 1 / 7"), "{rendered}");
    let progress = row_text(&screen, row_containing(&screen, "○"));
    assert_eq!(progress.matches('●').count(), 1, "{progress}");
    assert_eq!(progress.matches('○').count(), 6, "{progress}");
    assert!(
        rendered.contains("This setup configures agents and local source metadata"),
        "{rendered}"
    );
    assert!(!rendered.contains("previews every filesystem mutation"));
    assert!(!rendered.contains("Skip"), "{rendered}");
    assert!(!rendered.contains("demo"), "{rendered}");
}

#[test]
fn setup_progress_has_text_and_semantic_styles_for_every_state() {
    let harness = Harness::new();
    let mut app = harness.first_run();
    app.update(Action::Continue);
    app.update(Action::Continue);

    let screen = buffer(&app, 120, 40);
    let progress_row = row_containing(&screen, "○");
    let progress = row_text(&screen, progress_row);
    assert!(progress.contains('✓'), "{progress}");
    assert!(progress.contains('●'), "{progress}");
    assert!(progress.contains('○'), "{progress}");
    assert_eq!(
        style_in_row(&screen, progress_row, "✓").fg,
        Some(Color::Rgb(0x8b, 0xd4, 0x9c))
    );
    assert_eq!(
        style_in_row(&screen, progress_row, "●").fg,
        Some(Color::Rgb(0x73, 0xd7, 0xee))
    );
    assert_eq!(
        style_in_row(&screen, progress_row, "○").fg,
        Some(Color::Rgb(0x84, 0x91, 0xa1))
    );
}

#[test]
fn detect_agents_separates_focus_selection_root_and_executable_status() {
    let harness = Harness::new();
    fs::create_dir_all(harness.directory.path().join("home/.agents/skills"))
        .expect("create detected Codex root");
    let mut app = harness.first_run();
    app.update(Action::Continue);
    app.update(Action::MoveSelection(1));
    app.update(Action::ToggleSelection);

    let screen = buffer(&app, 80, 24);
    let rendered = text(&screen);
    let codex_row = row_containing(&screen, "Codex");
    let row = row_text(&screen, codex_row);

    assert!(
        rendered.contains("Choose the agents Skilled should configure"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("All supported agents are selected"),
        "{rendered}"
    );
    assert!(row.contains("▌ [ ] Codex"), "{row}\n{rendered}");
    assert!(row.contains("✓ root found"), "{row}\n{rendered}");
    assert!(row.contains("- executable not found"), "{row}\n{rendered}");
    assert_eq!(
        style_in_row(&screen, codex_row, "✓ root found").fg,
        Some(Color::Rgb(0x8b, 0xd4, 0x9c))
    );
    assert_eq!(
        style_in_row(&screen, codex_row, "- executable not found").fg,
        Some(Color::Rgb(0x84, 0x91, 0xa1))
    );
    assert!(
        style_in_row(&screen, codex_row, "Codex")
            .add_modifier
            .contains(Modifier::BOLD)
    );
}

#[test]
fn placeholder_setup_steps_describe_only_observed_work() {
    let harness = Harness::new();
    let mut app = harness.first_run();
    app.update(Action::Continue);
    app.update(Action::Continue);

    let choose_roots = text(&buffer(&app, 80, 24));
    assert!(
        choose_roots.contains("scan roots not configured"),
        "{choose_roots}"
    );
    assert!(
        choose_roots.contains("has not scanned your home directory"),
        "{choose_roots}"
    );

    app.update(Action::Continue);
    let discovery = text(&buffer(&app, 80, 24));
    assert!(
        discovery.contains("automatic discovery unavailable"),
        "{discovery}"
    );
    assert!(discovery.contains("Registered sources: 0"), "{discovery}");

    app.update(Action::Continue);
    let catalogs = text(&buffer(&app, 80, 24));
    assert!(
        catalogs.contains("no catalogs awaiting confirmation"),
        "{catalogs}"
    );

    // Step six is where the roots are read, so its effect has to run for the
    // step to report anything.
    let update = app.update(Action::Continue);
    app.perform_effects(update.effects())
        .expect("installation scan");
    let installations = text(&buffer(&app, 80, 24));
    // It reports what it found at each documented path rather than announcing
    // that it cannot look.
    assert!(
        installations.contains("Skilled read the global skill root"),
        "{installations}"
    );
    for root in [
        "~/.claude/skills",
        "~/.agents/skills",
        "~/.config/opencode/skills",
    ] {
        assert!(installations.contains(root), "{root} in\n{installations}");
    }
    assert_eq!(installations.matches("root not found").count(), 3);
    assert!(
        installations.contains("nothing was changed"),
        "{installations}"
    );

    for screen in [choose_roots, discovery, catalogs, installations] {
        assert!(!screen.contains("Skip"), "{screen}");
        assert!(!screen.contains("demo"), "{screen}");
        assert!(!screen.contains("Doctor findings"), "{screen}");
    }
}

#[test]
fn the_setup_scan_step_names_the_reason_a_root_could_not_be_read() {
    let harness = Harness::new();
    let home = harness.directory.path().join("home");
    fs::create_dir_all(home.join(".claude")).expect("create Claude Code parent");
    fs::write(home.join(".claude/skills"), "not a directory")
        .expect("write a file where the root belongs");
    let mut app = harness.first_run();
    for _ in 0..4 {
        app.update(Action::Continue);
    }
    let update = app.update(Action::Continue);
    app.perform_effects(update.effects())
        .expect("installation scan");

    let screen = buffer(&app, 80, 24);
    let rendered = text(&screen);

    assert!(rendered.contains("STEP 6 / 7"), "{rendered}");
    assert!(rendered.contains("root unreadable"), "{rendered}");
    // A root that could not be read was attempted, not read; saying "read"
    // above the failure row would flatten the failure into a success.
    assert!(
        rendered.contains("Skilled attempted to read the global skill root"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("Skilled read the global skill root"),
        "{rendered}"
    );
    // The root contributed nothing, so its reason is the only account of it.
    let reason = row_containing(&screen, "the skill root is not a directory");
    let line = row_text(&screen, reason);
    assert!(line.contains("× Claude Code:"), "{line}");
}

#[test]
fn summary_names_inventory_as_the_next_destination_everywhere() {
    let harness = Harness::new();
    let mut app = harness.first_run();
    for _ in 0..6 {
        app.update(Action::Continue);
    }

    let screen = buffer(&app, 80, 24);
    let rendered = text(&screen);
    assert!(rendered.contains("Enter Inventory"), "{rendered}");
    assert!(
        row_text(&screen, 23).contains("Enter Inventory"),
        "{rendered}"
    );
    assert!(!rendered.contains("Enter Continue"), "{rendered}");

    app.update(Action::OpenHelp);
    let help = text(&buffer(&app, 80, 24));
    assert!(help.contains("Enter Inventory"), "{help}");
    assert!(help.contains("enter the Inventory view"), "{help}");
}

#[test]
fn settings_explains_and_frames_the_existing_rerun_effects() {
    let harness = Harness::new();
    let mut app = harness.completed_setup();
    app.update(Action::OpenSettings);

    let screen = buffer(&app, 80, 24);
    let rendered = text(&screen);
    assert!(rendered.contains("▌ Rerun setup"), "{rendered}");
    assert!(
        rendered.contains("Agent root and executable detection is refreshed"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Agent selections and registered sources are retained"),
        "{rendered}"
    );
    assert!(rendered.contains("No agent is launched"), "{rendered}");
    assert!(rendered.contains("Enter Rerun"), "{rendered}");
    assert!(rendered.contains("Esc Close"), "{rendered}");
    assert!(
        rendered.lines().any(|line| line.matches('─').count() > 30),
        "{rendered}"
    );
    for y in 2..23 {
        for x in (0..6).chain(74..80) {
            assert_eq!(
                screen[(x, y)].symbol(),
                " ",
                "workspace text leaked outside Settings at ({x}, {y})\n{rendered}"
            );
        }
    }
}

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
    // The lock note is status, not a disabled entry, so it must render in the
    // readable muted tone rather than the faint decorative one.
    assert_eq!(
        style_in_row(&screen, 1, "locked during setup").fg,
        Some(Color::Rgb(0x84, 0x91, 0xa1))
    );
    let rendered = text(&screen);
    assert!(rendered.contains("Enter Register"), "{rendered}");
    assert!(rendered.contains("Esc Cancel"), "{rendered}");
    assert!(!rendered.contains("Enter Continue"), "{rendered}");
}

#[test]
fn the_empty_state_styles_its_glyph_headline_and_body_distinctly() {
    let harness = Harness::new();
    let screen = buffer(&harness.completed_setup(), 80, 24);

    assert_eq!(
        style_at(&screen, "⌕").fg,
        Some(Color::Rgb(0x53, 0x61, 0x71))
    );
    let headline = style_at(&screen, "No agent skill root exists yet");
    assert_eq!(headline.fg, Some(Color::Rgb(0xd7, 0xde, 0xe7)));
    assert!(headline.add_modifier.contains(Modifier::BOLD));

    let body = style_at(&screen, "Skilled looked for the documented");
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
    fs::create_dir_all(repository.join("skills/second")).expect("create second skill fixture");
    fs::write(
        repository.join("skills/second/SKILL.md"),
        "---\nname: second\ndescription: Second fixture\n---\n# Second\n",
    )
    .expect("write second skill fixture");
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "add second skill"]);
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    app.update(Action::OpenSources);
    let screen = buffer(&app, 120, 40);

    const TERMINAL: Color = Color::Rgb(0x0b, 0x0f, 0x14);
    const BAND: Color = Color::Rgb(0x0d, 0x12, 0x18);
    const SURFACE: Color = Color::Rgb(0x0f, 0x15, 0x1d);
    const SURFACE_2: Color = Color::Rgb(0x12, 0x1a, 0x24);
    const SURFACE_3: Color = Color::Rgb(0x17, 0x21, 0x2c);

    // The canvas shows through the workspace, while the two chrome rows sit on
    // their own band.
    assert_eq!(style_in_row(&screen, 0, "skilled").bg, Some(BAND));
    assert_eq!(
        style_in_row(&screen, row_containing(&screen, "Repositories"), "┌").bg,
        Some(TERMINAL)
    );
    // The title row is laid out as two rectangles, so the band is only right
    // if it covers the empty space between them and the far end of the second
    // one as well as the text in the first.
    let title = row_text(&screen, 0);
    let status = u16::try_from(
        title[..title.find('●').expect("session status glyph")]
            .chars()
            .count(),
    )
    .expect("column");
    let last = screen.area.x + screen.area.width - 1;
    assert_eq!(
        screen[(screen.area.x + status - 2, 0)].style().bg,
        Some(BAND),
        "the gap between the product mark and the session status is on the band"
    );
    assert_eq!(
        screen[(last, 0)].style().bg,
        Some(BAND),
        "the band should reach the end of the row, not stop with the product half"
    );

    // The navigation strip is its own band, with the active tab lifted.
    // Sources is the active tab here, so Inventory is the inactive probe.
    assert_eq!(style_in_row(&screen, 1, "1 Inventory").bg, Some(SURFACE));
    assert_eq!(style_in_row(&screen, 1, "▌Sources").bg, Some(SURFACE_2));

    // The key-hint row shares the title bar's band, and it reaches the edge of
    // the terminal rather than stopping where the last hint does.
    let key_hints = screen.area.y + screen.area.height - 1;
    assert_eq!(style_in_row(&screen, key_hints, "Quit").bg, Some(BAND));
    assert_eq!(
        screen[(last, key_hints)].style().bg,
        Some(BAND),
        "the band should reach the end of the row, not stop at the last hint"
    );
    // A key cap keeps its own emphasis on top of that band.
    assert_eq!(style_in_row(&screen, key_hints, "q ").bg, Some(SURFACE_2));

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
    let branch = row_containing(&screen, "Branch: main   HEAD:");
    assert_eq!(style_in_row(&screen, branch, "Branch:").bg, Some(SURFACE));
    let worktree = row_containing(&screen, "Worktree:");
    assert_eq!(
        style_in_row(&screen, worktree, "✓ clean").bg,
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
    // Every root of this home was found absent, so the scan is complete but
    // read nothing. That earns the same refusal the Inventory surfaces give:
    // a phrase, not a measured zero.
    assert!(!screen.contains("Installed:"), "{screen}");
    assert!(
        screen.contains("installation counts unavailable: no skill root was read"),
        "{screen}"
    );
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
        "Tab / Shift-Tab Region",
        "a Add source",
        "1 Inventory",
        "Esc Back one region",
        "? Help",
        "q Quit",
    ] {
        assert!(
            sources_help.contains(command),
            "missing {command:?} in\n{sources_help}"
        );
    }
    assert!(!sources_help.contains("j/k Move"), "{sources_help}");
    assert!(!sources_help.contains("Enter Open"), "{sources_help}");

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

    assert!(
        row.contains("Esc Back one region"),
        "{row}\n{}",
        text(&screen)
    );
    assert!(text(&screen).contains("1 Inventory"));
}

#[test]
fn sources_key_hints_follow_the_focused_region_at_compact_size() {
    let empty_harness = Harness::new();
    let mut empty = empty_harness.completed_setup();
    empty.update(Action::OpenSources);
    let empty_footer = row_text(&buffer(&empty, 80, 24), 23);
    assert!(
        empty_footer.contains("Tab/Shift-Tab Region"),
        "{empty_footer}"
    );
    assert!(!empty_footer.contains("j/k Move"), "{empty_footer}");
    assert!(!empty_footer.contains("Enter Open"), "{empty_footer}");
    assert!(empty_footer.contains("Esc Back"), "{empty_footer}");

    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    create_source_fixture(&repository);
    fs::create_dir_all(repository.join("skills/second")).expect("create second skill fixture");
    fs::write(
        repository.join("skills/second/SKILL.md"),
        "---\nname: second\ndescription: Second fixture\n---\n# Second\n",
    )
    .expect("write second skill fixture");
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "add second skill"]);
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    app.update(Action::OpenSources);

    let repositories = row_text(&buffer(&app, 80, 24), 23);
    assert!(
        repositories.contains("Tab/Shift-Tab Region"),
        "{repositories}"
    );
    assert!(!repositories.contains("j/k Move"), "{repositories}");
    assert!(repositories.contains("Enter Open"), "{repositories}");
    assert!(repositories.contains("Esc Back"), "{repositories}");

    app.update(Action::AdvanceSourcesPane);
    let variants = row_text(&buffer(&app, 80, 24), 23);
    assert!(variants.contains("j/k Move"), "{variants}");
    assert!(variants.contains("Enter Open"), "{variants}");
    assert!(variants.contains("Esc Back"), "{variants}");

    app.update(Action::AdvanceSourcesPane);
    let details = row_text(&buffer(&app, 80, 24), 23);
    assert!(details.contains("Tab/Shift-Tab Region"), "{details}");
    assert!(!details.contains("j/k Move"), "{details}");
    assert!(!details.contains("Enter Open"), "{details}");
    assert!(details.contains("Esc Back"), "{details}");
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

    let contexts: [(&SkilledApp, &str); 3] = [
        (
            &setup,
            " j/k Move   Space Toggle   Enter Continue   Esc Back   ? Help   q Quit",
        ),
        (
            &inventory,
            " Tab/Shift-Tab Region   2 Sources   s Settings   ? Help   q Quit",
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

    assert_eq!(
        row_text(&buffer(&sources, 80, 24), 23),
        " Tab/Shift-Tab Region   a Add source   1 Inventory   ? Help   q Quit   Esc Back"
    );
    assert_eq!(
        row_text(&buffer(&sources, 120, 40), 39),
        " Tab/Shift-Tab Region   a Add source   1 Inventory   ? Help   q Quit   Esc Back"
    );
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
fn an_inventory_with_no_root_to_read_says_so_without_inventing_a_result() {
    let harness = Harness::new();
    let app = harness.completed_setup();

    let screen = text(&buffer(&app, 80, 24));

    assert!(screen.contains("Global inventory"), "{screen}");
    // No root was read, so there is nothing to count and the subtitle says
    // that rather than reporting a zero the scan never observed.
    assert!(screen.contains("no root read"), "{screen}");
    assert!(!screen.contains("0 skills"), "{screen}");
    assert!(!screen.contains("nothing installed"), "{screen}");
    assert!(
        screen.contains("No agent skill root exists yet"),
        "{screen}"
    );
    assert!(
        screen.contains("Skilled looked for the documented global skill root"),
        "{screen}"
    );
    assert!(screen.contains("It did not create one"), "{screen}");
    // The Roots line accounts for each agent, so absence is legible.
    assert!(
        screen.contains("Roots: Claude Code no root · Codex no root · OpenCode no root"),
        "{screen}"
    );

    // Doctor, updates, installation, and repair do not exist yet, so nothing
    // may report their results or offer their actions.
    for invented in ["Doctor findings", "Uninstall", "Repair", "Update available"] {
        assert!(!screen.contains(invented), "{invented} in\n{screen}");
    }
    // No per-skill status either: there are no skills to carry one.
    for glyph in ["✓", "×", "U "] {
        assert!(!screen.contains(glyph), "{glyph} in\n{screen}");
    }
}

#[test]
fn a_root_that_exists_but_holds_nothing_is_distinguished_from_a_missing_one() {
    let harness = Harness::new();
    fs::create_dir_all(harness.directory.path().join("home/.claude/skills"))
        .expect("create an empty Claude Code root");
    let app = harness.completed_setup();

    let screen = text(&buffer(&app, 80, 24));

    assert!(screen.contains("No skills are installed"), "{screen}");
    assert!(screen.contains("hold no skill directories"), "{screen}");
    assert!(
        screen.contains("Roots: Claude Code 0 installed · Codex no root · OpenCode no root"),
        "{screen}"
    );
}

#[test]
fn deselecting_every_agent_says_nothing_was_looked_at_rather_than_nothing_exists() {
    let harness = Harness::new();
    // A root that does exist, to prove the copy is about the selection and not
    // about what is on disk.
    fs::create_dir_all(harness.directory.path().join("home/.claude/skills"))
        .expect("create a Claude Code root");
    let mut app = harness.first_run();
    app.update(Action::Continue);
    for _ in 0..3 {
        app.update(Action::ToggleSelection);
        app.update(Action::MoveSelection(1));
    }
    for _ in 0..6 {
        let update = app.update(Action::Continue);
        app.perform_effects(update.effects())
            .expect("perform setup effects");
    }

    let screen = buffer(&app, 80, 24);
    let rendered = text(&screen);

    assert!(rendered.contains("No agent is configured"), "{rendered}");
    assert!(
        rendered.contains("Skilled reads the skill root of the agents chosen"),
        "{rendered}"
    );
    // Skilled did not look, so it may not report on what exists.
    assert!(
        !rendered.contains("No agent skill root exists yet"),
        "{rendered}"
    );
    assert!(!rendered.contains("No skills are installed"), "{rendered}");
    assert!(
        rendered.contains("Roots: Claude Code not selected"),
        "{rendered}"
    );
    let roots = row_starting_with(&screen, "Roots:");
    assert_eq!(
        style_in_row(&screen, roots, "not selected").fg,
        Some(Color::Rgb(0x84, 0x91, 0xa1))
    );
}

#[test]
fn an_unreadable_root_names_its_reason_in_a_critical_tone() {
    let harness = Harness::new();
    let home = harness.directory.path().join("home");
    fs::create_dir_all(home.join(".claude")).expect("create Claude Code parent");
    fs::write(home.join(".claude/skills"), "not a directory")
        .expect("write a file where the root belongs");
    let app = harness.completed_setup();

    let screen = buffer(&app, 80, 24);
    let rendered = text(&screen);

    // The reason is the only account of a root that contributed nothing, so it
    // is on screen, in words, beside a glyph — not colour alone.
    let reason = row_containing(&screen, "the skill root is not a directory");
    let line = row_text(&screen, reason);
    assert!(line.starts_with("× Claude Code:"), "{line}");
    assert_eq!(
        style_in_row(&screen, reason, "×").fg,
        Some(Color::Rgb(0xee, 0x6b, 0x73))
    );
    // The count is withheld, because no count was observed.
    assert!(rendered.contains("not fully read"), "{rendered}");
    assert!(!rendered.contains("nothing installed"), "{rendered}");
    // The header rule survives the extra line rather than being displaced.
    assert!(rendered.contains("──────"), "{rendered}");
    assert!(
        rendered.contains("An agent skill root could not be read"),
        "{rendered}"
    );
}

#[test]
fn a_partially_read_inventory_lists_rows_without_claiming_a_total() {
    let harness = Harness::new();
    let home = harness.directory.path().join("home");
    write_skill_fixture(&home.join(".claude/skills/alpha"), "alpha");
    fs::create_dir_all(home.join(".agents")).expect("create Codex root parent");
    fs::write(home.join(".agents/skills"), "not a directory")
        .expect("write a file where the root belongs");
    let app = harness.completed_setup();

    let rendered = text(&buffer(&app, 80, 24));

    // One root was read and holds a skill, but another could not be read, so
    // the subtitle describes what is listed rather than claiming a total that
    // would read as covering every root.
    assert!(rendered.contains("1 listed · not fully read"), "{rendered}");
    assert!(!rendered.contains("1 skill"), "{rendered}");
    assert!(
        rendered.contains("the skill root is not a directory"),
        "{rendered}"
    );
}

/// A tab count is a claim about a scan, so it is withheld whenever the scan
/// read nothing, or did not read everything it was asked to.
///
/// The navigation must reach the same verdict as the subtitle beside the
/// inventory, which is why each case checks both.
#[test]
fn navigation_withholds_a_count_it_could_not_observe() {
    // What follows a tab's title, past the space that separates them, which is
    // where a count would appear. `None` means the row ends there, which the
    // last title's does: nothing follows it to inspect.
    fn after(row: &str, title: &str) -> Option<char> {
        let index = row
            .find(title)
            .unwrap_or_else(|| panic!("{title:?} not found in {row:?}"));
        let rest = &row[index + title.len()..];
        rest.strip_prefix(' ').unwrap_or(rest).chars().next()
    }

    // A file where a root belongs is a root that cannot be read.
    fn block_root(at: PathBuf) {
        fs::create_dir_all(at.parent().expect("root parent")).expect("create root parent");
        fs::write(&at, "not a directory").expect("write a file where the root belongs");
    }

    /// A home to arrange, paired with the phrase the inventory subtitle uses
    /// for the state it produces.
    type Scenario = (fn(&Path), &'static str);

    let scenarios: [Scenario; 3] = [
        // Nothing was read, so there is nothing to count.
        (|_home| {}, "no root read"),
        (
            |home| block_root(home.join(".claude/skills")),
            "not fully read",
        ),
        // One root reads cleanly and holds a skill; the other does not, so a
        // total would cover less than it appears to.
        (
            |home| {
                write_skill_fixture(&home.join(".claude/skills/alpha"), "alpha");
                block_root(home.join(".agents/skills"));
            },
            "1 listed · not fully read",
        ),
    ];

    for (prepare, subtitle) in scenarios {
        let harness = Harness::new();
        let home = harness.directory.path().join("home");
        prepare(&home);
        let app = harness.completed_setup();

        let screen = buffer(&app, 80, 24);
        let navigation = row_text(&screen, 1);
        let rendered = text(&screen);

        // The subtitle names this state, and the count beside the tab is
        // absent in it. Two spaces after the title is the positive evidence:
        // nothing at all was rendered between it and the next entry.
        assert!(rendered.contains(subtitle), "{rendered}");
        assert!(navigation.contains("▌Inventory  2 Sources"), "{navigation}");
        assert_eq!(
            after(&navigation, "▌Inventory"),
            // The next entry's own marker, and so nothing of the inventory's.
            Some(' '),
            "{subtitle:?} may not state a total: {navigation}"
        );
        // The registry is not the filesystem: it is still fully known.
        assert!(navigation.contains(" 2 Sources 0 "), "{navigation}");

        // A destination this release cannot open counts nothing, and says so
        // by rendering nothing rather than by a placeholder that reads as an
        // empty measurement.
        for unavailable in ["Updates (soon)", "Doctor (soon)"] {
            match after(&navigation, unavailable) {
                // Doctor is the last entry, so the row ends after its title.
                None => assert!(navigation.ends_with(unavailable), "{navigation}"),
                Some(next) => {
                    assert!(!next.is_ascii_digit(), "{unavailable}: {navigation}");
                    assert_ne!(next, '—', "{unavailable}: {navigation}");
                }
            }
        }
        assert!(!navigation.contains('—'), "{navigation}");
    }
}

/// The one state in which a bare zero beside the tab is an observation: a root
/// that exists, was read, and holds nothing.
#[test]
fn navigation_states_zero_when_a_root_was_read_and_held_nothing() {
    let harness = Harness::new();
    fs::create_dir_all(harness.directory.path().join("home/.claude/skills"))
        .expect("create an empty root");
    let app = harness.completed_setup();

    let screen = buffer(&app, 80, 24);
    let navigation = row_text(&screen, 1);

    // "nothing installed" and "0" are the same finding, worded for their
    // places. Neither may appear when no root was read.
    assert!(
        text(&screen).contains("nothing installed"),
        "{}",
        text(&screen)
    );
    assert!(navigation.contains("▌Inventory 0 "), "{navigation}");
}

#[test]
fn wide_terminals_gain_a_detail_region_and_compact_ones_do_not() {
    let harness = Harness::new();
    let app = harness.completed_setup();

    let wide = text(&buffer(&app, 120, 40));
    assert!(wide.contains("Details"), "{wide}");
    assert!(wide.contains("no selection"), "{wide}");
    assert!(wide.contains("Nothing to show"), "{wide}");
    assert!(wide.contains("Identity, provenance, and"), "{wide}");
    // Both regions are present, so the primary empty state still reads.
    assert!(wide.contains("No agent skill root exists yet"), "{wide}");

    let compact = text(&buffer(&app, 80, 24));
    assert!(!compact.contains("Nothing to show"), "{compact}");
    assert!(
        compact.contains("No agent skill root exists yet"),
        "{compact}"
    );
}

#[test]
fn sources_wide_workspace_shows_three_regions_and_marks_focus_in_text() {
    let harness = Harness::new();
    let mut app = harness.completed_setup();
    app.update(Action::OpenSources);

    let rendered = text(&buffer(&app, 120, 40));

    assert!(rendered.contains("▌ Repositories"), "{rendered}");
    assert!(rendered.contains("Available variants"), "{rendered}");
    assert!(rendered.contains("Details"), "{rendered}");
}

#[test]
fn sources_compact_workspace_replaces_regions_as_enter_advances() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    create_source_fixture(&repository);
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    app.update(Action::OpenSources);

    let repositories = text(&buffer(&app, 80, 24));
    assert!(repositories.contains("▌ Repositories"), "{repositories}");
    assert!(
        !repositories.contains("Available variants"),
        "{repositories}"
    );
    assert!(!repositories.contains("Details"), "{repositories}");

    app.update(Action::AdvanceSourcesPane);
    let variants = text(&buffer(&app, 80, 24));
    assert!(variants.contains("▌ Available variants"), "{variants}");
    assert!(!variants.contains("Repositories"), "{variants}");
    assert!(!variants.contains("Details"), "{variants}");

    app.update(Action::AdvanceSourcesPane);
    let details = text(&buffer(&app, 80, 24));
    assert!(details.contains("▌ Details"), "{details}");
    assert!(!details.contains("Repositories"), "{details}");
    assert!(!details.contains("Available variants"), "{details}");

    app.update(Action::Back);
    let variants = text(&buffer(&app, 80, 24));
    assert!(variants.contains("▌ Available variants"), "{variants}");
    app.update(Action::Back);
    let repositories = text(&buffer(&app, 80, 24));
    assert!(repositories.contains("▌ Repositories"), "{repositories}");
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

    assert!(rendered.contains("✓ valid portable"), "{rendered}");
    assert_eq!(
        style_at(&screen, "✓ valid").fg,
        Some(Color::Rgb(0x8b, 0xd4, 0x9c))
    );

    assert!(rendered.contains("× invalid broken"), "{rendered}");
    assert_eq!(
        style_at(&screen, "× invalid").fg,
        Some(Color::Rgb(0xee, 0x6b, 0x73))
    );
}

#[test]
fn sources_distinguish_clean_and_unavailable_repositories_semantically() {
    let clean_harness = Harness::new();
    let clean_repository = clean_harness.directory.path().join("clean-source");
    create_source_fixture(&clean_repository);
    let mut clean_app = clean_harness.completed_setup();
    let preview = clean_app
        .preview_source(&clean_repository)
        .expect("preview clean source");
    clean_app
        .confirm_source(preview)
        .expect("register clean source");
    clean_app.update(Action::OpenSources);
    let clean = buffer(&clean_app, 120, 40);
    assert!(text(&clean).contains("✓ clean"), "{}", text(&clean));
    assert_eq!(
        style_at(&clean, "✓ clean").fg,
        Some(Color::Rgb(0x8b, 0xd4, 0x9c))
    );

    let unavailable_harness = Harness::new();
    let repository = unavailable_harness.directory.path().join("source");
    let moved = unavailable_harness.directory.path().join("source-moved");
    create_source_fixture(&repository);
    let environment = unavailable_harness.environment();
    let mut app = unavailable_harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    drop(app);
    fs::rename(&repository, &moved).expect("move registered checkout");
    let mut reopened = SkilledApp::open(environment).expect("reopen application");
    reopened.update(Action::OpenSources);

    let unavailable = buffer(&reopened, 120, 40);
    let rendered = text(&unavailable);
    assert!(rendered.contains("× unavailable"), "{rendered}");
    assert!(rendered.contains("Source error:"), "{rendered}");
    assert!(!rendered.contains("✓ clean"), "{rendered}");
    assert_eq!(
        style_at(&unavailable, "× unavailable").fg,
        Some(Color::Rgb(0xee, 0x6b, 0x73))
    );
}

#[test]
fn sources_surface_catalog_scan_errors_in_variants_and_details() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    create_source_fixture(&repository);
    let environment = harness.environment();
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    drop(app);
    let connection =
        rusqlite::Connection::open(harness.directory.path().join("data/skilled.sqlite3"))
            .expect("open application database");
    connection
        .execute("UPDATE catalog_roots SET relative_path = '../outside'", [])
        .expect("create unsafe stored catalog fixture");
    drop(connection);
    let mut reopened = SkilledApp::open(environment).expect("reopen application");
    reopened.update(Action::OpenSources);
    reopened.update(Action::AdvanceSourcesPane);

    let variants = text(&buffer(&reopened, 80, 24));
    assert!(variants.contains("× unavailable"), "{variants}");
    assert!(variants.contains("Open Details"), "{variants}");
    assert!(variants.contains("scan unavailable"), "{variants}");
    assert!(!variants.contains("0 found"), "{variants}");

    reopened.update(Action::AdvanceSourcesPane);
    let details = text(&buffer(&reopened, 80, 24));
    assert!(details.contains("Catalog error: ../outside:"), "{details}");
    assert!(details.contains("relative"), "{details}");
}

#[test]
fn details_keep_failed_catalog_errors_beside_a_healthy_selected_variant() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    for (catalog, skill) in [("catalog-a", "portable"), ("catalog-b", "second")] {
        let directory = repository.join(catalog).join("codex/skills").join(skill);
        fs::create_dir_all(&directory).expect("create catalog fixture");
        fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: {skill}\ndescription: {skill} fixture\n---\n# Fixture\n"),
        )
        .expect("write catalog fixture");
    }
    git(&repository, &["init", "-b", "main"]);
    git(&repository, &["config", "user.name", "Skilled Test"]);
    git(
        &repository,
        &["config", "user.email", "skilled@example.test"],
    );
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "fixture"]);
    let environment = harness.environment();
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    assert_eq!(preview.catalogs().len(), 2);
    app.confirm_source(preview).expect("register source");
    drop(app);
    let connection =
        rusqlite::Connection::open(harness.directory.path().join("data/skilled.sqlite3"))
            .expect("open application database");
    connection
        .execute(
            "UPDATE catalog_roots SET relative_path = '../outside' WHERE relative_path LIKE 'catalog-b/%'",
            [],
        )
        .expect("corrupt one stored catalog path");
    drop(connection);
    let mut reopened = SkilledApp::open(environment).expect("reopen application");
    reopened.update(Action::OpenSources);
    reopened.update(Action::AdvanceSourcesPane);
    reopened.update(Action::AdvanceSourcesPane);

    let details = text(&buffer(&reopened, 80, 24));

    assert!(details.contains("Directory: portable"), "{details}");
    assert!(details.contains("Status: ✓ valid"), "{details}");
    assert!(details.contains("Catalog error: ../outside:"), "{details}");
    assert!(details.contains("relative"), "{details}");
}

#[test]
fn long_branch_catalog_and_candidate_paths_cannot_hide_selected_variant_status() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    let catalog = "catalog-segment-with-a-long-name/codex/skills";
    let candidate = "portable-candidate-with-a-long-but-valid-directory-name";
    let directory = repository.join(catalog).join(candidate);
    fs::create_dir_all(&directory).expect("create long catalog fixture");
    fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {candidate}\ndescription: Long candidate fixture\n---\n# Fixture\n"),
    )
    .expect("write long candidate fixture");
    let branch = format!("feature/{}", "long-branch-segment-".repeat(8));
    git(&repository, &["init", "-b", &branch]);
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
    app.update(Action::AdvanceSourcesPane);
    app.update(Action::AdvanceSourcesPane);

    let details = text(&buffer(&app, 80, 24));

    assert!(
        details.contains("Directory: portable-candidate"),
        "{details}"
    );
    assert!(details.contains("Status: ✓ valid"), "{details}");
    assert!(
        details.contains("Description: Long candidate fixture"),
        "{details}"
    );
}

#[test]
fn sources_details_render_stored_repository_catalog_and_variant_metadata() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    create_source_fixture(&repository);
    git(
        &repository,
        &["remote", "add", "origin", "https://example.test/source.git"],
    );
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    let head = app.sources()[0].head().to_owned();
    let canonical = repository.canonicalize().expect("canonical repository");
    app.update(Action::OpenSources);
    app.update(Action::AdvanceSourcesPane);
    app.update(Action::AdvanceSourcesPane);

    let rendered = text(&buffer(&app, 80, 24));

    for expected in [
        "REPOSITORY",
        "Label: source",
        "Branch: main",
        "Status: ✓ clean",
        "Remote: https://example.test/source.git",
        "Last scan:",
        "CATALOG",
        "Classification: Common",
        "Compatibility: Claude Code: yes · Codex: yes · OpenCode: yes",
        "VARIANT",
        "Directory: portable · Name: portable",
        "Path: skills/portable",
        "Status: ✓ valid",
        "Description: Portable fixture",
    ] {
        assert!(
            rendered.contains(expected),
            "missing {expected:?} in\n{rendered}"
        );
    }
    assert!(rendered.contains(&head), "{rendered}");
    assert!(
        rendered.contains(&canonical.display().to_string()),
        "{rendered}"
    );
}

#[test]
fn sources_keep_offscreen_repository_selection_visible() {
    let harness = Harness::new();
    let mut app = harness.completed_setup();
    for index in 0..24 {
        let repository = harness.directory.path().join(format!("source-{index:02}"));
        create_source_fixture(&repository);
        let preview = app.preview_source(&repository).expect("preview source");
        app.confirm_source(preview).expect("register source");
    }
    app.update(Action::OpenSources);

    let rendered = text(&buffer(&app, 80, 24));

    assert_eq!(app.focused_source(), 23);
    assert!(rendered.contains("▌ source-23"), "{rendered}");
    assert!(!rendered.contains("source-00"), "{rendered}");
}

#[test]
fn long_wrapped_metadata_keeps_variant_identity_and_status_visible() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    fs::create_dir_all(repository.join("skills/portable")).expect("create skill directory");
    fs::write(
        repository.join("skills/portable/SKILL.md"),
        format!(
            "---\nname: portable\ndescription: Long detail {}\n---\n# Portable\n",
            "description ".repeat(40)
        ),
    )
    .expect("write long skill metadata");
    git(&repository, &["init", "-b", "main"]);
    git(&repository, &["config", "user.name", "Skilled Test"]);
    git(
        &repository,
        &["config", "user.email", "skilled@example.test"],
    );
    git(
        &repository,
        &[
            "remote",
            "add",
            "origin",
            &format!("https://example.test/{}", "remote-segment/".repeat(16)),
        ],
    );
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "fixture"]);
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    app.update(Action::OpenSources);
    app.update(Action::AdvanceSourcesPane);
    app.update(Action::AdvanceSourcesPane);

    let rendered = text(&buffer(&app, 80, 24));

    assert!(
        rendered.contains("Directory: portable · Name: portable"),
        "{rendered}"
    );
    assert!(rendered.contains("Status: ✓ valid"), "{rendered}");
    assert!(rendered.contains("remote-segment/..."), "{rendered}");
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
fn add_source_uses_the_shared_dialog_body_divider_and_footer() {
    let harness = Harness::new();
    let mut app = harness.completed_setup();
    app.update(Action::OpenSources);
    app.update(Action::BeginAddSource);

    let screen = buffer(&app, 80, 24);
    let rendered = text(&screen);

    assert!(rendered.contains("Local Git repository"), "{rendered}");
    assert!(
        rendered.contains("Read-only checkout and catalog scan"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Esc Cancel   Enter Inspect"),
        "{rendered}"
    );
    assert_eq!(
        rendered.matches("Enter Inspect").count(),
        2,
        "the dialog and global key-hint bar should both name the required action\n{rendered}"
    );
    assert!(
        rendered.contains("────────────────────────────────"),
        "the dialog footer needs a visible divider\n{rendered}"
    );
}

#[test]
fn add_source_keeps_a_wrapped_inspection_error_and_actions_visible_at_minimum_size() {
    let harness = Harness::new();
    let mut app = harness.completed_setup();
    app.update(Action::OpenSources);
    app.update(Action::BeginAddSource);
    let missing = harness
        .directory
        .path()
        .join("a-deliberately-long-missing-repository-directory")
        .join("and-an-equally-long-nested-path");
    for character in missing.to_string_lossy().chars() {
        app.update(Action::AppendSourcePath(character));
    }
    let update = app.update(Action::SubmitSourcePath);
    app.perform_effects(update.effects())
        .expect("failed inspection remains recoverable");

    assert!(app.source_path_input_active());
    assert!(app.source_error().is_some());
    let rendered = text(&buffer(&app, 80, 24));

    assert!(rendered.contains("×"), "{rendered}");
    assert!(rendered.contains("No such file"), "{rendered}");
    assert!(
        rendered.contains("Esc Cancel   Enter Inspect"),
        "{rendered}"
    );
}

#[test]
fn catalog_confirmation_names_repository_and_catalog_metadata_without_abbreviations() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    create_source_fixture(&repository);
    git(
        &repository,
        &[
            "remote",
            "add",
            "origin",
            "https://secret@example.test/team/source.git",
        ],
    );
    let mut app = harness.completed_setup();
    app.update(Action::OpenSources);
    app.update(Action::BeginAddSource);
    for character in repository.to_string_lossy().chars() {
        app.update(Action::AppendSourcePath(character));
    }
    let update = app.update(Action::SubmitSourcePath);
    app.perform_effects(update.effects())
        .expect("inspect source");
    let preview = app.pending_source().expect("pending source");
    let head = &preview.inspected().head()[..8];

    let rendered = text(&buffer(&app, 120, 40));

    assert!(
        rendered.contains(&format!(
            "Repository: {}",
            repository
                .canonicalize()
                .expect("canonical repository")
                .display()
        )),
        "{rendered}"
    );
    assert!(rendered.contains("Branch: main"), "{rendered}");
    assert!(rendered.contains(&format!("HEAD: {head}")), "{rendered}");
    assert!(
        rendered.contains("Remote: https://example.test/team/source.git"),
        "{rendered}"
    );
    assert!(rendered.contains("Worktree: ✓ clean"), "{rendered}");
    assert!(
        rendered.contains("▌ Included · skills · 1 candidate"),
        "{rendered}"
    );
    assert!(rendered.contains("Common catalog"), "{rendered}");
    assert!(rendered.contains("Claude Code: yes"), "{rendered}");
    assert!(rendered.contains("Codex: yes"), "{rendered}");
    assert!(rendered.contains("OpenCode: yes"), "{rendered}");
    for unsupported in ["Install", "Forget", "Rescan", "operation plan", "toast"] {
        assert!(
            !rendered.contains(unsupported),
            "{unsupported}:\n{rendered}"
        );
    }
}

#[test]
fn catalog_confirmation_uses_one_shared_body_and_owner_dialog_footer() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    create_source_fixture(&repository);

    let mut sources = harness.completed_setup();
    sources.update(Action::OpenSources);
    sources.update(Action::BeginAddSource);
    for character in repository.to_string_lossy().chars() {
        sources.update(Action::AppendSourcePath(character));
    }
    let update = sources.update(Action::SubmitSourcePath);
    sources
        .perform_effects(update.effects())
        .expect("inspect source from Sources");

    let setup_harness = Harness::new();
    let mut setup = setup_harness.first_run();
    for _ in 0..3 {
        setup.update(Action::Continue);
    }
    setup.update(Action::BeginAddSource);
    for character in repository.to_string_lossy().chars() {
        setup.update(Action::AppendSourcePath(character));
    }
    let update = setup.update(Action::SubmitSourcePath);
    setup
        .perform_effects(update.effects())
        .expect("inspect source from Setup");

    for (owner, app) in [("Sources", &sources), ("Setup", &setup)] {
        let rendered = text(&buffer(app, 80, 24));
        assert!(
            rendered.contains("Registration records metadata only"),
            "{owner}:\n{rendered}"
        );
        assert!(
            rendered.contains("Esc Cancel   Enter Register"),
            "{owner}:\n{rendered}"
        );
        assert!(
            !rendered.contains("Enter registers metadata only"),
            "body commands should not duplicate the owner footer for {owner}:\n{rendered}"
        );
    }

    let sources_rendered = text(&buffer(&sources, 80, 24));
    assert_eq!(
        sources_rendered.matches("Enter Register").count(),
        2,
        "the Sources dialog and global key-hint bar should both keep Register visible\n{sources_rendered}"
    );
}

#[test]
fn catalog_confirmation_bounds_pathological_paths_without_hiding_required_sections() {
    let harness = Harness::new();
    let repository = harness
        .directory
        .path()
        .join(format!("source-{}", "r".repeat(120)));
    for index in 0..2 {
        let name = format!("skill-{index}");
        let first = format!("set-{index}-{}", "a".repeat(170));
        let second = format!("nested-{index}-{}", "b".repeat(165));
        let skill = repository
            .join("catalogs")
            .join(first)
            .join(second)
            .join("claude-code/skills")
            .join(&name);
        fs::create_dir_all(&skill).expect("create long catalog fixture");
        fs::write(
            skill.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: fixture\n---\n# Fixture\n"),
        )
        .expect("write long catalog fixture");
    }
    git(&repository, &["init", "-b", "main"]);
    git(&repository, &["config", "user.name", "Skilled Test"]);
    git(
        &repository,
        &["config", "user.email", "skilled@example.test"],
    );
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "fixture"]);
    let long_branch = format!("branch-{}", "c".repeat(120));
    git(&repository, &["branch", "-m", &long_branch]);
    let long_remote = format!("https://example.test/{}/source.git", "d".repeat(180));
    git(&repository, &["remote", "add", "origin", &long_remote]);

    let mut app = harness.completed_setup();
    app.update(Action::OpenSources);
    app.update(Action::BeginAddSource);
    for character in repository.to_string_lossy().chars() {
        app.update(Action::AppendSourcePath(character));
    }
    let update = app.update(Action::SubmitSourcePath);
    app.perform_effects(update.effects())
        .expect("inspect long source");
    app.update(Action::ToggleCatalogIncluded);
    app.update(Action::MoveCatalogSelection(1));
    app.update(Action::ToggleCatalogIncluded);
    app.update(Action::ConfirmPendingSource);

    let screen = buffer(&app, 80, 24);
    let rendered = text(&screen);

    assert!(rendered.contains("Repository:"), "{rendered}");
    assert!(rendered.contains("source-"), "{rendered}");
    assert!(rendered.contains("Worktree: ✓ clean"), "{rendered}");
    assert!(rendered.contains("▌ Excluded"), "{rendered}");
    let focused_catalog = row_containing(&screen, "▌ Excluded");
    assert_eq!(
        style_in_row(&screen, focused_catalog, "▌").fg,
        Some(Color::Rgb(0x73, 0xd7, 0xee)),
        "{rendered}"
    );
    assert!(rendered.contains("set-1-"), "{rendered}");
    assert!(rendered.contains("Agent-specific"), "{rendered}");
    assert!(rendered.contains("Claude Code: yes"), "{rendered}");
    assert!(rendered.contains("Codex: no"), "{rendered}");
    assert!(rendered.contains("OpenCode: no"), "{rendered}");
    assert!(
        rendered.contains("Select at least one catalog root to register."),
        "{rendered}"
    );
    assert!(
        rendered.contains("Registration records metadata only"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Esc Cancel   Enter Register"),
        "{rendered}"
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

    /// One healthy skill installed for two agents, one dangling link, and one
    /// physical copy, with the OpenCode root absent entirely.
    ///
    /// The checkout lives inside the temporary home so every rendered path is
    /// home-relative and stable.
    #[cfg(unix)]
    fn installed_inventory(&self) -> SkilledApp {
        let home = self.directory.path().join("home");
        let repository = home.join("library");
        for skill in ["alpha", "beta"] {
            write_skill_fixture(&repository.join("skills").join(skill), skill);
        }
        git(&repository, &["init", "-b", "main"]);
        git(&repository, &["config", "user.name", "Skilled Test"]);
        git(
            &repository,
            &["config", "user.email", "skilled@example.test"],
        );
        git(&repository, &["add", "."]);
        git(&repository, &["commit", "-m", "fixture"]);

        let mut app = self.completed_setup();
        let preview = app.preview_source(&repository).expect("preview source");
        app.confirm_source(preview).expect("register source");

        let claude = home.join(".claude/skills");
        let codex = home.join(".agents/skills");
        fs::create_dir_all(&claude).expect("create Claude Code root");
        fs::create_dir_all(&codex).expect("create Codex root");
        symlink(repository.join("skills/alpha"), claude.join("alpha"));
        symlink(home.join("gone"), claude.join("broken"));
        symlink(repository.join("skills/alpha"), codex.join("alpha"));
        write_skill_fixture(&codex.join("copied"), "copied");

        app.update(Action::OpenSources);
        let update = app.update(Action::OpenInventory);
        app.perform_effects(update.effects())
            .expect("installation scan");
        app
    }
}

#[cfg(unix)]
fn symlink(target: PathBuf, at: PathBuf) {
    std::os::unix::fs::symlink(target, at).expect("install symbolic link");
}

fn write_skill_fixture(directory: &Path, name: &str) {
    fs::create_dir_all(directory).expect("create skill fixture");
    fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {name} fixture\n---\n# {name}\n"),
    )
    .expect("write skill fixture");
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

/// The style of the cell immediately after `needle` in row `y`.
///
/// A count is only ever a digit, so it has to be located by what precedes it
/// rather than by its own text.
///
/// Like [`style_in_row`], this counts one column per character: the chrome it
/// probes is single width, and a double-width glyph before the needle would
/// put the probe a column short.
fn style_following(buffer: &Buffer, y: u16, needle: &str) -> Style {
    let row = row_text(buffer, y);
    let byte_index = row
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not found in row {y}: {row:?}"));
    let column = row[..byte_index].chars().count() + needle.chars().count();
    buffer[(buffer.area.x + u16::try_from(column).expect("column"), y)].style()
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

/// Rendering that needs a real installed skill.
///
/// Managed installations are symbolic links, so these fixtures are
/// Unix-only.
#[cfg(unix)]
mod installed {
    use super::*;

    #[test]
    fn every_inventory_cell_pairs_its_colour_with_a_glyph_and_a_word() {
        let harness = Harness::new();
        let app = harness.installed_inventory();

        let screen = buffer(&app, 80, 24);
        let rendered = text(&screen);

        // Each state appears as a glyph in the agent columns and as a glyph plus a
        // word in the Health column, so nothing depends on colour alone.
        for (name, glyph, word, colour) in [
            ("alpha", "✓", "healthy", Color::Rgb(0x8b, 0xd4, 0x9c)),
            ("broken", "×", "broken", Color::Rgb(0xee, 0x6b, 0x73)),
            ("copied", "U", "unmanaged", Color::Rgb(0xc7, 0x9b, 0xf2)),
        ] {
            let row = row_containing(&screen, name);
            let line = row_text(&screen, row);
            assert!(line.contains(glyph), "{glyph} missing from {line:?}");
            assert!(
                line.contains(&format!("{glyph} {word}")),
                "{glyph} {word} missing from {line:?}"
            );
            assert_eq!(
                style_in_row(&screen, row, glyph).fg,
                Some(colour),
                "wrong tone for {name}"
            );
        }

        // An agent with no installation of a row shows the inactive dash, in the
        // readable muted tone rather than the faint decorative one.
        let alpha = row_containing(&screen, "alpha");
        assert_eq!(
            style_in_row(&screen, alpha, "-").fg,
            Some(Color::Rgb(0x84, 0x91, 0xa1))
        );
        assert!(rendered.contains("SKILL "), "{rendered}");
        for heading in ["CLAUDE", "CODEX", "OPENCODE", "HEALTH", "SOURCE"] {
            assert!(rendered.contains(heading), "{heading} in\n{rendered}");
        }
    }

    /// A placeholder in the Source column is not a source name, so it is set
    /// back in the muted tone; a real source label keeps the body text.
    #[test]
    fn the_source_column_sets_back_what_is_not_a_source_name() {
        const MUTED: Color = Color::Rgb(0x84, 0x91, 0xa1);
        const TEXT: Color = Color::Rgb(0xd7, 0xde, 0xe7);

        let harness = Harness::new();
        let app = harness.installed_inventory();

        let screen = buffer(&app, 80, 24);

        let copied = row_containing(&screen, "copied");
        assert_eq!(
            style_in_row(&screen, copied, "not registered").fg,
            Some(MUTED),
            "an unregistered row should not read as a source name"
        );

        let alpha = row_containing(&screen, "alpha");
        assert_eq!(
            style_in_row(&screen, alpha, "library").fg,
            Some(TEXT),
            "a registered source label keeps the body text"
        );
    }

    #[test]
    fn navigation_counts_what_the_scan_and_the_registry_know() {
        const SURFACE: Color = Color::Rgb(0x0f, 0x15, 0x1d);
        const SURFACE_2: Color = Color::Rgb(0x12, 0x1a, 0x24);
        const AMBER: Color = Color::Rgb(0xe6, 0xbd, 0x6a);

        let harness = Harness::new();
        let mut app = harness.installed_inventory();
        // A root was read here, so a count is an observation the navigation is
        // entitled to state.
        let skills = app
            .inventory()
            .stated_skill_count()
            .expect("a scan that read a root states a count");
        let sources = app.sources().len();

        let screen = buffer(&app, 120, 40);
        let navigation = row_text(&screen, 1);

        // The count says the same thing the Inventory subtitle does: skills,
        // not every listed entry.
        assert!(
            navigation.contains(&format!("▌Inventory {skills} ")),
            "{navigation}"
        );
        assert!(
            navigation.contains(&format!(" 2 Sources {sources} ")),
            "{navigation}"
        );

        // A count carries no surface of its own: it sits inside its entry and
        // inherits it, so only the accent distinguishes it from the title. The
        // active tab's underline runs under its count as well, standing in for
        // the prototype's border along the whole tab, but the bold belongs to
        // the title and stops there.
        let inventory_count = style_following(&screen, 1, "▌Inventory ");
        assert_eq!(inventory_count.fg, Some(AMBER));
        assert_eq!(inventory_count.bg, Some(SURFACE_2));
        assert!(
            inventory_count.add_modifier.contains(Modifier::UNDERLINED),
            "the active tab's underline should span its count"
        );
        assert!(
            !inventory_count.add_modifier.contains(Modifier::BOLD),
            "the title's emphasis should not leak into the count"
        );
        let sources_count = style_following(&screen, 1, "2 Sources ");
        assert_eq!(sources_count.fg, Some(AMBER));
        assert_eq!(sources_count.bg, Some(SURFACE));
        assert!(!sources_count.add_modifier.contains(Modifier::UNDERLINED));
        assert!(!sources_count.add_modifier.contains(Modifier::BOLD));

        // Which is what makes both swap when the other tab is active.
        app.update(Action::OpenSources);
        let screen = buffer(&app, 120, 40);
        let inventory_count = style_following(&screen, 1, "1 Inventory ");
        assert_eq!(inventory_count.fg, Some(AMBER));
        assert_eq!(inventory_count.bg, Some(SURFACE));
        assert!(!inventory_count.add_modifier.contains(Modifier::UNDERLINED));
        assert!(!inventory_count.add_modifier.contains(Modifier::BOLD));
        let sources_count = style_following(&screen, 1, "▌Sources ");
        assert_eq!(sources_count.fg, Some(AMBER));
        assert_eq!(sources_count.bg, Some(SURFACE_2));
        assert!(sources_count.add_modifier.contains(Modifier::UNDERLINED));
        assert!(!sources_count.add_modifier.contains(Modifier::BOLD));
    }

    /// Stray content is listed, but never described as a skill.
    #[test]
    fn a_root_holding_only_stray_files_is_not_described_as_holding_skills() {
        let harness = Harness::new();
        let root = harness.directory.path().join("home/.claude/skills");
        fs::create_dir_all(&root).expect("create a Claude Code root");
        fs::write(root.join("README.md"), "notes").expect("write a stray file");
        fs::write(root.join(".DS_Store"), "litter").expect("write platform litter");
        let app = harness.completed_setup();

        let screen = buffer(&app, 80, 24);
        let rendered = text(&screen);

        // The three counts on screen describe the same roots and must agree.
        assert!(
            rendered.contains("0 skills · 2 other entries"),
            "{rendered}"
        );
        // The tab counts skills, so two stray entries are not two of anything
        // it may report.
        let navigation = row_text(&screen, 1);
        assert!(navigation.contains("▌Inventory 0 "), "{navigation}");
        assert!(
            rendered.contains("Roots: Claude Code 0 installed"),
            "{rendered}"
        );
        assert!(!rendered.contains("2 skills"), "{rendered}");
        // "unmanaged" describes content Skilled could own; a README is not it.
        assert!(!rendered.contains("unmanaged"), "{rendered}");
        let row = row_containing(&screen, "README.md");
        let line = row_text(&screen, row);
        assert!(line.contains("- not a skill"), "{line}");
        assert_eq!(
            style_in_row(&screen, row, "- not a skill").fg,
            Some(Color::Rgb(0x84, 0x91, 0xa1))
        );
    }

    /// Detail that outgrows its region says so in words and in a tone, and
    /// keeps the reason an installation is broken ahead of its path.
    #[test]
    fn a_truncated_detail_region_reports_the_cut_and_keeps_the_findings() {
        let harness = Harness::new();
        let mut app = harness.installed_inventory();
        app.update(Action::MoveInventorySelection(1));
        app.update(Action::AdvanceInventoryPane);

        // The reason an installation is broken comes before its path, so it
        // is the last thing a short region gives up rather than the first.
        let broken = text(&buffer(&app, 80, 24));
        let finding = broken
            .find("Finding: install.dangling_symlink")
            .unwrap_or_else(|| panic!("{broken}"));
        let path = broken
            .find("Path: ~/.claude/skills/broken")
            .unwrap_or_else(|| panic!("{broken}"));
        assert!(finding < path, "{broken}");

        // A row installed for two agents outgrows the region, and says so.
        app.update(Action::Back);
        app.update(Action::MoveInventorySelection(-1));
        app.update(Action::AdvanceInventoryPane);
        let screen = buffer(&app, 80, 24);
        let notice = row_containing(&screen, "more line");
        let line = row_text(&screen, notice);
        assert!(line.contains("! "), "{line}");
        assert!(line.contains("widen or lengthen the terminal"), "{line}");
        assert_eq!(
            style_in_row(&screen, notice, "!").fg,
            Some(Color::Rgb(0xe6, 0xbd, 0x6a))
        );
    }

    /// An entry whose type could not be read never claims to know what it is.
    #[test]
    fn an_unreadable_entry_renders_without_asserting_its_type() {
        let harness = Harness::new();
        let root = harness.directory.path().join("home/.claude/skills");
        fs::create_dir_all(root.join("opaque")).expect("create an entry to seal");
        let mut app = harness.completed_setup();
        // Readable but not searchable: the listing succeeds while stat on each
        // child fails, so the type is genuinely never observed.
        fs::set_permissions(&root, fs::Permissions::from_mode(0o444))
            .expect("drop search permission");
        let update = app.update(Action::OpenSources);
        app.perform_effects(update.effects()).expect("effects");
        let update = app.update(Action::OpenInventory);
        app.perform_effects(update.effects()).expect("scan");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).expect("restore permissions");
        if app.inventory().rows().is_empty() {
            // Permission bits do not bind the superuser.
            return;
        }
        app.update(Action::AdvanceInventoryPane);

        let screen = buffer(&app, 80, 24);
        let rendered = text(&screen);

        assert!(rendered.contains("Object: could not be read"), "{rendered}");
        assert!(!rendered.contains("Object: not a directory"), "{rendered}");
        assert!(
            rendered.contains("Validation: - not attempted"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Finding: install.unreadable_entry"),
            "{rendered}"
        );
    }

    #[test]
    fn the_roots_line_explains_what_a_dash_cell_means() {
        let harness = Harness::new();
        let app = harness.installed_inventory();

        let screen = buffer(&app, 80, 24);
        let row = row_starting_with(&screen, "Roots:");
        let line = row_text(&screen, row);

        assert!(line.contains("Claude Code 2 installed"), "{line}");
        assert!(line.contains("Codex 2 installed"), "{line}");
        assert!(line.contains("OpenCode no root"), "{line}");
        // A scanned root reads as healthy; an absent one is inactive, not an error.
        assert_eq!(
            style_in_row(&screen, row, "2 installed").fg,
            Some(Color::Rgb(0x8b, 0xd4, 0x9c))
        );
        assert_eq!(
            style_in_row(&screen, row, "no root").fg,
            Some(Color::Rgb(0x84, 0x91, 0xa1))
        );
    }
    #[test]
    fn the_selected_installation_is_marked_and_its_detail_follows_the_selection() {
        let harness = Harness::new();
        let mut app = harness.installed_inventory();

        let first = text(&buffer(&app, 120, 40));
        assert!(first.contains("▌ alpha"), "{first}");
        assert!(first.contains("Details  alpha"), "{first}");
        assert!(first.contains("Source: library"), "{first}");
        // Each agent section names the variant it carries, so two agents
        // installing the same name from different sources cannot be collapsed.
        assert!(first.contains("Variant: skills · skills/alpha"), "{first}");
        assert!(first.contains("Path: ~/.claude/skills/alpha"), "{first}");
        assert!(first.contains("Object: symbolic link"), "{first}");

        app.update(Action::MoveInventorySelection(1));
        let second = text(&buffer(&app, 120, 40));
        assert!(second.contains("▌ broken"), "{second}");
        assert!(second.contains("Details  broken"), "{second}");
        // Unresolved content is reported, never adopted.
        assert!(
            second.contains("Not resolved to any registered"),
            "{second}"
        );
        assert!(
            second.contains("Finding: install.dangling_symlink"),
            "{second}"
        );
        assert!(second.contains("critical"), "{second}");
        assert!(second.contains("the link target"), "{second}");
        assert!(second.contains("Validation: - not attempted"), "{second}");
    }
    #[test]
    fn the_filter_bar_shows_the_query_and_the_narrowed_count() {
        let harness = Harness::new();
        let mut app = harness.installed_inventory();
        app.update(Action::BeginInventoryFilter);
        for character in "cop".chars() {
            app.update(Action::AppendInventoryFilter(character));
        }

        let screen = buffer(&app, 80, 24);
        let rendered = text(&screen);

        assert!(rendered.contains("/cop▌"), "{rendered}");
        assert!(rendered.contains("1 of 3 listed"), "{rendered}");
        assert!(rendered.contains("copied"), "{rendered}");
        assert!(!rendered.contains("alpha"), "{rendered}");
        // The filter takes every printable key, so the navigation row says so
        // instead of advertising destination digits that would be typed as text.
        assert!(
            rendered.contains("navigation is locked while the filter is open"),
            "{rendered}"
        );
        assert_eq!(
            row_text(&screen, 23),
            " Enter Apply   Esc Clear   Ctrl-C Quit"
        );
    }
    #[test]
    fn compact_terminals_drill_into_the_detail_region_and_back_out() {
        let harness = Harness::new();
        let mut app = harness.installed_inventory();

        let table = text(&buffer(&app, 80, 24));
        assert!(table.contains("▌ Global inventory"), "{table}");
        assert!(!table.contains("Object: symbolic link"), "{table}");

        app.update(Action::AdvanceInventoryPane);
        let detail = text(&buffer(&app, 80, 24));
        assert!(detail.contains("▌ Details  alpha"), "{detail}");
        assert!(detail.contains("Object: symbolic link"), "{detail}");
        assert!(!detail.contains("Global inventory"), "{detail}");
        // Neither selection nor filtering acts in the detail region — the
        // query box is drawn above the table, which is not on screen — so the
        // bar must not advertise either.
        assert_eq!(
            row_text(&buffer(&app, 80, 24), 23),
            " Tab/Shift-Tab Region   2 Sources   s Settings   ? Help   q Quit   Esc Back"
        );

        app.update(Action::Back);
        let back = text(&buffer(&app, 80, 24));
        assert!(back.contains("▌ Global inventory"), "{back}");
    }
}
