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

use skilled::{Action, AgentKind, AppEnvironment, InventoryPane, SkilledApp, tui::RenderFeedback};

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
        style_in_row(
            &screen,
            row_containing(&screen, "Repositories"),
            "Repositories"
        )
        .bg,
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
    // The band must reach the end of the pane, which is what distinguishes it
    // from a label-length smear. The pane is unboxed, so it ends at the rule
    // column dividing it from the variants beside it.
    let row = row_text(&screen, focused);
    let divider = row
        .char_indices()
        .find(|(_, character)| *character == '│')
        .map(|(index, _)| u16::try_from(row[..index].chars().count()).expect("column"))
        .expect("Repositories region divider");
    assert_eq!(
        screen[(divider - 1, focused)].style().bg,
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

    // A view without an implementation is visibly unavailable rather than
    // absent, and offers no shortcut, because 3 is unmapped everywhere.
    assert!(navigation.contains("Updates (soon)"), "{navigation}");
    assert!(!navigation.contains("3 Updates"), "{navigation}");
    // Doctor is implemented, so it carries its route like any other.
    assert!(navigation.contains(" 4 Doctor"), "{navigation}");
    assert!(!navigation.contains("Doctor (soon)"), "{navigation}");

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
            " Tab/Shift-Tab Region   2 Sources   4 Doctor   s Settings   ? Help   q Quit",
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

    // Sources advertises one route more than its row holds at eighty columns,
    // so the two hints that matter least give way and the overflow mark says
    // they did. The way out survives, which is what the budget is for.
    assert_eq!(
        row_text(&buffer(&sources, 80, 24), 23),
        " Tab/Shift-Tab Region   a Add source   1 Inventory   4 Doctor   Esc Back …"
    );
    assert_eq!(
        row_text(&buffer(&sources, 120, 40), 39),
        " Tab/Shift-Tab Region   a Add source   1 Inventory   4 Doctor   ? Help   q Quit   Esc Back"
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
        assert!(navigation.contains(" 2 Sources ·0 "), "{navigation}");

        // A destination this release cannot open counts nothing, and says so
        // by rendering nothing rather than by a placeholder that reads as an
        // empty measurement. The '·' lead-in is reserved for real counts, so
        // an unavailable tab may not borrow it either.
        for unavailable in ["Updates (soon)"] {
            match after(&navigation, unavailable) {
                None => assert!(navigation.ends_with(unavailable), "{navigation}"),
                Some(next) => {
                    assert!(!next.is_ascii_digit(), "{unavailable}: {navigation}");
                    assert_ne!(next, '—', "{unavailable}: {navigation}");
                    assert_ne!(next, '·', "{unavailable}: {navigation}");
                }
            }
        }
        // Doctor lists findings observed from the same roots, so it withholds
        // its count in exactly the states the Inventory withholds its own.
        // It is the last entry, so nothing follows its title.
        assert_eq!(
            after(&navigation, "4 Doctor"),
            None,
            "{subtitle:?} may not state a finding total: {navigation}"
        );
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
    assert!(navigation.contains("▌Inventory ·0 "), "{navigation}");
}

/// Entering the Inventory and the scan that fills it are two moments: the
/// reducer changes the view, and the scan is the effect performed after.
/// Between them the screen may not rest on a scan taken for the view the user
/// was just looking at — the only honest thing it can say is "not scanned".
#[test]
fn inventory_between_its_transition_and_its_scan_says_not_scanned() {
    let harness = Harness::new();
    let mut app = harness.first_run();
    for _ in 0..6 {
        let update = app.update(Action::Continue);
        app.perform_effects(update.effects())
            .expect("perform setup effects");
    }

    // The last Continue leaves Setup for the Inventory; its scan effect has
    // not been performed yet.
    let update = app.update(Action::Continue);
    let screen = buffer(&app, 80, 24);
    let rendered = text(&screen);
    let navigation = row_text(&screen, 1);

    let header = row_text(&screen, row_containing(&screen, "Global inventory"));
    assert!(header.contains("not scanned"), "{header}");
    let roots = row_text(&screen, row_containing(&screen, "Roots:"));
    assert_eq!(roots.matches("not scanned").count(), 3, "{roots}");
    assert!(
        rendered.contains("Installation roots have not been scanned"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Skilled scans the roots when this view opens."),
        "{rendered}"
    );
    // No count may stand beside the tab for a scan that has not happened.
    assert!(
        navigation.contains("▌Inventory  2 Sources ·0 "),
        "{navigation}"
    );
    // Nothing has been listed, so nothing may be hinted as movable, openable,
    // or filterable. The hint row is located by its one constant entry.
    let hints = row_text(&screen, row_containing(&screen, "Quit"));
    for hint in ["Move", "Open", "Filter"] {
        assert!(!hints.contains(hint), "{hint}: {hints}");
    }
    // The wide detail region has no selection to describe either.
    let wide = text(&buffer(&app, 120, 40));
    assert!(wide.contains("Nothing to show"), "{wide}");

    // Once the effect lands the screen reports the scan, not the gap.
    app.perform_effects(update.effects()).expect("perform scan");
    let rendered = text(&buffer(&app, 80, 24));
    assert!(!rendered.contains("not scanned"), "{rendered}");
    assert!(rendered.contains("no root read"), "{rendered}");
}

/// A filter survives switching away and back, so in the gap before the rescan
/// lands it is the one stale claim left: "0 of 0 listed" would say skills were
/// read and hidden, and "No skills match the filter" would promise installed
/// skills to show again — over a snapshot nothing has been read into. The
/// scan state is the more fundamental fact and outranks the filter.
#[test]
fn a_surviving_filter_does_not_speak_for_the_unscanned_gap() {
    let harness = Harness::new();
    write_skill_fixture(
        &harness.directory.path().join("home/.claude/skills/alpha"),
        "alpha",
    );
    let mut app = harness.completed_setup();
    app.update(Action::BeginInventoryFilter);
    app.update(Action::AppendInventoryFilter('z'));
    app.update(Action::SubmitInventoryFilter);

    app.update(Action::OpenSources);
    let update = app.update(Action::OpenInventory);
    let screen = buffer(&app, 80, 24);
    let rendered = text(&screen);

    let header = row_text(&screen, row_containing(&screen, "Global inventory"));
    assert!(header.contains("not scanned"), "{header}");
    assert!(!rendered.contains("0 of 0 listed"), "{rendered}");
    assert!(
        rendered.contains("Installation roots have not been scanned"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("No skills match the filter"),
        "{rendered}"
    );

    // The filter is not discarded: once the scan lands it narrows the fresh
    // rows, and says so against a snapshot that exists.
    app.perform_effects(update.effects()).expect("perform scan");
    let rendered = text(&buffer(&app, 80, 24));
    assert!(rendered.contains("0 of 1 listed"), "{rendered}");
    assert!(
        rendered.contains("No skills match the filter"),
        "{rendered}"
    );
}

/// The same gap exists on every path into the Inventory, not only at the end
/// of setup: switching over from Sources returns the scan effect too, and
/// until it is performed the scan taken the last time the Inventory was on
/// screen is just as stale.
#[test]
fn returning_to_the_inventory_says_not_scanned_until_the_scan_lands() {
    let harness = Harness::new();
    fs::create_dir_all(harness.directory.path().join("home/.claude/skills"))
        .expect("create an empty root");
    let mut app = harness.completed_setup();
    // A scan exists to be stale: the empty root was read as "nothing
    // installed", which the gap must not keep showing.
    assert!(text(&buffer(&app, 80, 24)).contains("nothing installed"));

    app.update(Action::OpenSources);
    let update = app.update(Action::OpenInventory);

    let screen = buffer(&app, 80, 24);
    let rendered = text(&screen);
    let header = row_text(&screen, row_containing(&screen, "Global inventory"));
    assert!(header.contains("not scanned"), "{header}");
    assert!(!rendered.contains("nothing installed"), "{rendered}");
    assert!(
        rendered.contains("Installation roots have not been scanned"),
        "{rendered}"
    );

    app.perform_effects(update.effects()).expect("perform scan");
    let rendered = text(&buffer(&app, 80, 24));
    assert!(rendered.contains("nothing installed"), "{rendered}");
}

/// A deselected agent is already terminal for the coming scan. The gap must
/// label it "not selected" — not "not scanned" — while still saying the
/// selected roots have not been read yet.
#[test]
fn gap_with_a_deselected_agent_keeps_selection_honest() {
    let harness = Harness::new();
    fs::create_dir_all(harness.directory.path().join("home/.claude/skills"))
        .expect("create an empty Claude Code root");
    let mut app = harness.first_run();
    app.update(Action::Continue);
    // DetectAgents focuses Claude Code first; move to Codex and deselect it.
    app.update(Action::MoveSelection(1));
    app.update(Action::ToggleSelection);
    for _ in 0..6 {
        let update = app.update(Action::Continue);
        app.perform_effects(update.effects())
            .expect("perform setup effects");
    }
    assert!(text(&buffer(&app, 80, 24)).contains("nothing installed"));

    app.update(Action::OpenSources);
    let update = app.update(Action::OpenInventory);
    let screen = buffer(&app, 80, 24);
    let rendered = text(&screen);
    let header = row_text(&screen, row_containing(&screen, "Global inventory"));
    let roots = row_text(&screen, row_containing(&screen, "Roots:"));

    assert!(header.contains("not scanned"), "{header}");
    assert!(
        rendered.contains("Installation roots have not been scanned"),
        "{rendered}"
    );
    assert!(roots.contains("Claude Code not scanned"), "{roots}");
    assert!(roots.contains("Codex not selected"), "{roots}");
    assert!(roots.contains("OpenCode not scanned"), "{roots}");
    assert!(!roots.contains("Codex not scanned"), "{roots}");
    // No count: the selected roots have not been read.
    let navigation = row_text(&screen, 1);
    assert!(
        navigation.contains("▌Inventory  2 Sources ·0 "),
        "{navigation}"
    );

    app.perform_effects(update.effects()).expect("perform scan");
    let screen = buffer(&app, 80, 24);
    let roots = row_text(&screen, row_containing(&screen, "Roots:"));
    assert!(roots.contains("Codex not selected"), "{roots}");
    assert!(!roots.contains("not scanned"), "{roots}");
    assert!(
        text(&screen).contains("nothing installed"),
        "{}",
        text(&screen)
    );
}

/// When nothing is selected there is nothing to scan. The gap must not pretend
/// roots are pending — it should already say no agent is configured.
#[test]
fn gap_with_every_agent_deselected_does_not_claim_not_scanned() {
    let harness = Harness::new();
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

    app.update(Action::OpenSources);
    let update = app.update(Action::OpenInventory);
    let screen = buffer(&app, 80, 24);
    let rendered = text(&screen);
    let header = row_text(&screen, row_containing(&screen, "Global inventory"));
    let navigation = row_text(&screen, 1);

    assert!(!header.contains("not scanned"), "{header}");
    // Same subtitle the post-scan all-deselected path already uses.
    assert!(header.contains("no root read"), "{header}");
    assert!(
        !rendered.contains("Installation roots have not been scanned"),
        "{rendered}"
    );
    assert!(rendered.contains("No agent is configured"), "{rendered}");
    let roots = row_text(&screen, row_containing(&screen, "Roots:"));
    assert_eq!(roots.matches("not selected").count(), 3, "{roots}");
    assert!(!roots.contains("not scanned"), "{roots}");
    assert!(
        navigation.contains("▌Inventory  2 Sources ·0 "),
        "{navigation}"
    );

    app.perform_effects(update.effects()).expect("perform scan");
    let rendered = text(&buffer(&app, 80, 24));
    assert!(rendered.contains("No agent is configured"), "{rendered}");
}

/// With three registered sources the navigation row reads
/// '▌Inventory 1 2 Sources 3  Updates (soon) ...'. A bare amber '3' two
/// cells from the next tab's title teaches two readings of one digit — the
/// count of the previous entry, or the route key of the next — and the
/// distinction would otherwise rest on colour alone. The lead-in glyph
/// '·N' makes the class textual: a count is always prefixed, so no bare
/// digit followed by a space and a title can be a count at all.
#[cfg(unix)]
#[test]
fn navigation_count_digit_cannot_read_as_a_route_key() {
    const AMBER: Color = Color::Rgb(0xe6, 0xbd, 0x6a);

    let harness = Harness::new();
    let home = harness.directory.path().join("home");
    // One skill in a real Claude Code root so Inventory can state a count,
    // and three distinct registered sources so Sources has to say '3'.
    write_skill_fixture(&home.join(".claude/skills/alpha"), "alpha");
    let mut app = harness.completed_setup();
    for source in ["library", "annex", "atelier"] {
        let repository = home.join(source);
        write_skill_fixture(&repository.join("skills/portable"), "portable");
        create_repository(&repository);
        let preview = app.preview_source(&repository).expect("preview source");
        app.confirm_source(preview).expect("register source");
    }

    let screen = buffer(&app, 80, 24);
    let navigation = row_text(&screen, 1);

    // Pin the exact neighbourhood the issue quotes — the active tab's count
    // on the left and the next tab's amber count on the right.
    assert!(
        navigation.contains("▌Inventory ·1  2 Sources ·3  Updates (soon)"),
        "{navigation}"
    );
    // The bare-digit form is the collision itself, so its absence is the
    // claim.
    assert!(
        !navigation.contains("Sources 3 ") && !navigation.contains("Inventory 1 2 Sources 3 "),
        "{navigation}"
    );

    // Grammar sweep: every ASCII digit in the row is either a count
    // (preceded immediately by '·') or a route key (followed by ' ' and an
    // available title). Anything else would be the old ambiguity. The sweep
    // operates on runs of digits, not digit-by-digit, so a multi-digit
    // count like '·12' is still classified as one count rather than failing
    // the trailing '2' on the leading-1 test.
    let mut chars = navigation.char_indices().peekable();
    while let Some((start, character)) = chars.next() {
        if !character.is_ascii_digit() {
            continue;
        }
        // Walk the rest of the digit run.
        while chars.peek().is_some_and(|(_, next)| next.is_ascii_digit()) {
            chars.next();
        }
        let prefix_is_count = navigation[..start].ends_with('·');
        let suffix_starts_route_key = navigation[start..]
            .strip_prefix(|character: char| character.is_ascii_digit())
            .unwrap_or(&navigation[start..])
            .strip_prefix(' ')
            .is_some_and(|rest| {
                rest.starts_with("Inventory")
                    || rest.starts_with("Sources")
                    || rest.starts_with("Doctor")
            });
        // `start` is the byte index of the run's first digit; after the
        // walk above the run's first digit is still at `start` and the run
        // ends at the byte index of the next non-digit char.
        let end = navigation[start..]
            .find(|character: char| !character.is_ascii_digit())
            .map_or(navigation.len(), |offset| start + offset);
        assert!(
            prefix_is_count || suffix_starts_route_key,
            "ambiguous digit run at {start}..{end} in {navigation:?}"
        );
    }

    // The dot inherits the count's amber foreground and drops the bold the
    // digits drop — one token, not a punctuation mark beside a separate one.
    let dot_style = style_at(&screen, "·3");
    assert_eq!(dot_style.fg, Some(AMBER));
    assert!(!dot_style.add_modifier.contains(Modifier::BOLD));

    // No count may leak onto an unavailable tab: the gap between
    // 'Updates (soon)' and the entry after it must not start with '·'.
    if let Some(after_updates) = navigation
        .find("Updates (soon)")
        .map(|position| position + "Updates (soon)".len())
    {
        let tail = &navigation[after_updates..];
        let before_next = tail.find("4 Doctor").unwrap_or(tail.len());
        assert!(
            !tail[..before_next].contains('·'),
            "'·' leaked onto an unavailable tab: {navigation}"
        );
    }

    // The lead-in survives the underline on an active tab.
    app.update(Action::OpenSources);
    let screen = buffer(&app, 80, 24);
    let navigation = row_text(&screen, 1);
    assert!(navigation.contains("▌Sources ·3"), "{navigation}");
    // Probe the cell right after the title — that is the '·' cell, which is
    // the first character of the count span and so carries the patched
    // style. Asking `style_at` for "▌Sources ·3" would land on the
    // marker's own style and the underline would pass trivially.
    let active_count_style = style_following(&screen, 1, "▌Sources ");
    assert_eq!(active_count_style.fg, Some(AMBER));
    assert!(
        active_count_style
            .add_modifier
            .contains(Modifier::UNDERLINED)
    );
    assert!(!active_count_style.add_modifier.contains(Modifier::BOLD));
}

/// Filter outrank is not only an all-selected story: a mixed gap still has
/// nothing a filter could have hidden, so "not scanned" beats "0 of 0 listed".
#[test]
fn a_surviving_filter_does_not_speak_for_a_mixed_unscanned_gap() {
    let harness = Harness::new();
    write_skill_fixture(
        &harness.directory.path().join("home/.claude/skills/alpha"),
        "alpha",
    );
    let mut app = harness.first_run();
    app.update(Action::Continue);
    app.update(Action::MoveSelection(1));
    app.update(Action::ToggleSelection);
    for _ in 0..6 {
        let update = app.update(Action::Continue);
        app.perform_effects(update.effects())
            .expect("perform setup effects");
    }
    app.update(Action::BeginInventoryFilter);
    app.update(Action::AppendInventoryFilter('z'));
    app.update(Action::SubmitInventoryFilter);

    app.update(Action::OpenSources);
    let update = app.update(Action::OpenInventory);
    let screen = buffer(&app, 80, 24);
    let rendered = text(&screen);
    let header = row_text(&screen, row_containing(&screen, "Global inventory"));
    let roots = row_text(&screen, row_containing(&screen, "Roots:"));

    assert!(header.contains("not scanned"), "{header}");
    assert!(!rendered.contains("0 of 0 listed"), "{rendered}");
    assert!(
        rendered.contains("Installation roots have not been scanned"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("No skills match the filter"),
        "{rendered}"
    );
    assert!(roots.contains("Codex not selected"), "{roots}");

    app.perform_effects(update.effects()).expect("perform scan");
    let rendered = text(&buffer(&app, 80, 24));
    assert!(rendered.contains("0 of 1 listed"), "{rendered}");
    assert!(
        rendered.contains("No skills match the filter"),
        "{rendered}"
    );
}

/// All-deselected is outside the filter's reach too: a query that survived
/// setup reset must not invent "0 of 0 listed" or promise installed skills
/// when no agent is configured — in the frozen gap and after the scan lands.
#[test]
fn a_surviving_filter_does_not_speak_when_no_agent_is_configured() {
    let harness = Harness::new();
    write_skill_fixture(
        &harness.directory.path().join("home/.claude/skills/alpha"),
        "alpha",
    );
    let mut app = harness.completed_setup();
    app.update(Action::BeginInventoryFilter);
    app.update(Action::AppendInventoryFilter('z'));
    app.update(Action::SubmitInventoryFilter);

    // Rerun setup, deselect every agent, and finish without clearing the query.
    app.update(Action::OpenSettings);
    let update = app.update(Action::RerunSetup);
    app.perform_effects(update.effects()).expect("reset setup");
    app.update(Action::Continue);
    for _ in 0..3 {
        app.update(Action::ToggleSelection);
        app.update(Action::MoveSelection(1));
    }
    for _ in 0..5 {
        let update = app.update(Action::Continue);
        app.perform_effects(update.effects())
            .expect("perform setup effects");
    }
    // Last Continue leaves Summary for Inventory; freeze the gap.
    let update = app.update(Action::Continue);
    let screen = buffer(&app, 80, 24);
    let rendered = text(&screen);
    let header = row_text(&screen, row_containing(&screen, "Global inventory"));

    assert!(header.contains("no root read"), "{header}");
    assert!(!rendered.contains("0 of 0 listed"), "{rendered}");
    assert!(
        !rendered.contains("No skills match the filter"),
        "{rendered}"
    );
    assert!(rendered.contains("No agent is configured"), "{rendered}");

    app.perform_effects(update.effects()).expect("perform scan");
    let screen = buffer(&app, 80, 24);
    let rendered = text(&screen);
    let header = row_text(&screen, row_containing(&screen, "Global inventory"));
    assert!(header.contains("no root read"), "{header}");
    assert!(!rendered.contains("0 of 0 listed"), "{rendered}");
    assert!(
        !rendered.contains("No skills match the filter"),
        "{rendered}"
    );
    assert!(rendered.contains("No agent is configured"), "{rendered}");
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

/// Sources reads as the same application as Inventory: no pane boxes, a rule
/// under every pane header, and a single column of vertical rule between one
/// region and the next.
#[test]
fn sources_regions_use_the_shared_unboxed_pane_scaffold() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    create_source_fixture(&repository);
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    app.update(Action::OpenSources);

    let screen = buffer(&app, 120, 40);
    let rendered = text(&screen);

    for corner in ['┌', '┐', '└', '┘'] {
        assert!(
            !rendered.contains(corner),
            "a boxed Sources region survived: {rendered}"
        );
    }

    // The header row carries all three headings, so the regions sit beside one
    // another with two rule columns dividing them.
    let header = row_starting_with(&screen, "▌ Repositories");
    let heading_row = row_text(&screen, header);
    assert!(heading_row.contains("Available variants"), "{heading_row}");
    assert!(heading_row.contains("Details"), "{heading_row}");
    assert_eq!(
        heading_row.matches('│').count(),
        2,
        "three regions need two dividers: {heading_row:?}"
    );
    assert!(
        row_text(&screen, header + 1).starts_with("───"),
        "{:?}",
        row_text(&screen, header + 1)
    );

    // The border that used to carry focus is gone, so the header has to carry
    // it: a cyan marker and an emphasised heading on the focused region, and
    // no marker at all on the regions beside it.
    const CYAN: Color = Color::Rgb(0x73, 0xd7, 0xee);
    const TEXT_STRONG: Color = Color::Rgb(0xf2, 0xf6, 0xfa);
    assert_eq!(style_in_row(&screen, header, "▌").fg, Some(CYAN));
    let heading = style_in_row(&screen, header, "Repositories");
    assert_eq!(heading.fg, Some(TEXT_STRONG));
    assert!(heading.add_modifier.contains(Modifier::BOLD), "{heading:?}");
    assert_eq!(
        heading_row.matches('▌').count(),
        1,
        "only the focused region is marked: {heading_row:?}"
    );
}

/// A repository entry carries the prototype's `.source-row` anatomy: the
/// label, the checkout it names, and the state it was last seen in.
#[test]
fn repository_entries_name_their_checkout_and_revision_beneath_their_label() {
    const MUTED: Color = Color::Rgb(0x84, 0x91, 0xa1);
    const CYAN: Color = Color::Rgb(0x73, 0xd7, 0xee);
    const SURFACE_3: Color = Color::Rgb(0x17, 0x21, 0x2c);

    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    create_source_fixture(&repository);
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    let head = app.sources()[0].head().to_owned();
    app.update(Action::OpenSources);

    let screen = buffer(&app, 120, 40);
    let label = row_starting_with(&screen, "▌ source");
    // Rows are read up to the rule that ends the pane, so the variants beside
    // it cannot satisfy an assertion about a repository entry.
    let pane_row = |y: u16| {
        let line = row_text(&screen, y);
        line[..line.find('│').expect("Repositories region divider")].to_owned()
    };
    let path = pane_row(label + 1);
    let state = pane_row(label + 2);

    // The path is bounded to the pane rather than wrapped, and set back: it
    // locates the checkout, so it is readable muted text and not faint.
    assert!(path.trim_end().ends_with("source"), "{path:?}");
    assert_eq!(style_in_row(&screen, label + 1, "/").fg, Some(MUTED));

    // The state line pairs the worktree badge with the branch and the short
    // revision Git itself prints.
    assert!(state.contains("✓ clean"), "{state:?}");
    assert!(state.contains(&format!("main@{}", &head[..7])), "{state:?}");
    assert!(
        !state.contains(&head),
        "the whole revision is not a row: {state:?}"
    );

    // The marker runs down every line of the selected entry, and the band
    // crosses the whole pane on each of them.
    let divider = row_text(&screen, label)
        .char_indices()
        .find(|(_, character)| *character == '│')
        .map(|(index, _)| {
            u16::try_from(row_text(&screen, label)[..index].chars().count()).expect("column")
        })
        .expect("Repositories region divider");
    for row in [label, label + 1, label + 2] {
        let line = row_text(&screen, row);
        assert!(line.starts_with("▌ "), "{line:?}");
        assert_eq!(style_in_row(&screen, row, "▌").fg, Some(CYAN), "{line:?}");
        assert_eq!(
            screen[(divider - 1, row)].style().bg,
            Some(SURFACE_3),
            "{line:?}"
        );
    }
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

/// Variants are read under the catalog that holds them, so the catalog path
/// is stated once as a group label instead of once per row.
#[test]
fn variants_are_grouped_under_a_label_naming_their_catalog() {
    const MUTED: Color = Color::Rgb(0x84, 0x91, 0xa1);
    const BAND: Color = Color::Rgb(0x0d, 0x12, 0x18);

    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    create_two_catalog_source_fixture(&repository);
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    app.update(Action::OpenSources);

    // Wide enough for the longer catalog path to be given whole; how a label
    // too long for its pane is shortened is the crossing test's subject.
    let screen = buffer(&app, 180, 40);

    // Each label names its catalog at the left, and states how the catalog is
    // classified and which agents it claims — the compatibility it actually
    // declares — against the far end of the label.
    let labelled = |path: &str, qualifiers: &str| {
        let row = row_containing(&screen, qualifiers);
        let label = row_text(&screen, row);
        let path_at = label.find(path).unwrap_or_else(|| panic!("{label:?}"));
        let qualifiers_at = label.find(qualifiers).expect("qualifiers");
        // Nothing but the gap between them: the qualifiers are set away from
        // the path, not appended to it.
        let gap = &label[path_at + path.len()..qualifiers_at];
        assert!(
            gap.len() >= 2 && gap.bytes().all(|byte| byte == b' '),
            "{label:?}"
        );
    };
    labelled("skills", "Common · all agents");
    labelled(
        "experimental/claude-code/skills",
        "Agent-specific · Claude Code",
    );

    // The label is muted text on the band the chrome already uses, so the
    // grouping reads without depending on colour alone.
    let grouped = row_containing(&screen, "Common · all agents");
    assert_eq!(style_in_row(&screen, grouped, "skills").fg, Some(MUTED));
    assert_eq!(style_in_row(&screen, grouped, "skills").bg, Some(BAND));

    // A variant row names its directory; the path it sat in is the label's
    // job now, so the parenthetical is gone.
    let variant = row_text(&screen, row_containing(&screen, "✓ valid portable"));
    assert!(!variant.contains("(skills/portable)"), "{variant:?}");
    assert!(!variant.contains("(experimental"), "{variant:?}");

    // A pane too narrow for all three keeps the path whole and sheds the
    // qualifiers, classification first: the path is which catalog, and the
    // rows beneath the label no longer carry that themselves. Which of the
    // qualifiers survives at a given width is the unit tests' subject; that
    // widening the pane only ever adds is the promise being kept here.
    //
    // Read only as far as the rule after the variants pane: the detail region
    // beside it states both facts for the catalog the selection rests in, and
    // would otherwise answer for the label.
    let label = |width: u16| {
        let screen = buffer(&app, width, 40);
        let row = row_text(
            &screen,
            row_containing(&screen, "experimental/claude-code/skills"),
        );
        row.split('│').nth(1).expect("variants region").to_owned()
    };
    assert!(!label(100).contains("Agent-specific"), "{:?}", label(100));
    assert!(!label(100).contains("Claude Code"), "{:?}", label(100));
    let wider = label(120);
    assert!(wider.contains("Claude Code"), "{wider:?}");
    assert!(!wider.contains("Agent-specific"), "{wider:?}");
    // Set away from the path, not appended to it.
    assert!(wider.contains("skills  "), "{wider:?}");

    // The promise is the pane's, not the terminal's. Widening from 99 to 100
    // relays the workspace — the variants pane goes from the whole terminal to
    // a third of it, and gives up the detail region's columns rather than the
    // label's — so the label says more at 99 than at 100. Recorded so that
    // crossing stays a deliberate one.
    app.update(Action::AdvanceSourcesPane);
    let compact = buffer(&app, 99, 40);
    let compact = row_text(
        &compact,
        row_containing(&compact, "experimental/claude-code/skills"),
    );
    assert!(
        compact.trim_end().ends_with("Agent-specific · Claude Code"),
        "{compact:?}"
    );
}

/// A variant row is bounded to its pane as well as to the name cap. At the
/// narrowest wide terminal the variants pane is 33 columns, and a row that
/// wrapped would carry its marker and the head of its band on one line and
/// the name they identify on the next.
#[test]
fn a_variant_row_is_bounded_to_its_pane_and_never_wraps() {
    const SURFACE_3: Color = Color::Rgb(0x17, 0x21, 0x2c);

    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    let variant = "portable-variant-with-a-very-long-directory-name";
    let directory = repository.join("skills").join(variant);
    fs::create_dir_all(&directory).expect("create variant fixture");
    fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {variant}\ndescription: Bounded fixture\n---\n# Fixture\n"),
    )
    .expect("write variant fixture");
    create_repository(&repository);
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    app.update(Action::OpenSources);

    let screen = buffer(&app, 100, 40);
    let selected = row_containing(&screen, "▌ ✓ valid");
    // The variants region of one row, read between the rules that divide the
    // three regions.
    let region = |y: u16| {
        let line = row_text(&screen, y);
        let dividers = line
            .char_indices()
            .filter(|(_, character)| *character == '│')
            .map(|(byte_index, _)| byte_index)
            .collect::<Vec<_>>();
        line[dividers[0] + '│'.len_utf8()..dividers[1]].to_owned()
    };

    let row = region(selected);
    assert!(
        row.trim_start().starts_with("▌ ✓ valid portable-variant"),
        "{row:?}"
    );
    assert!(
        region(selected + 1).trim().is_empty(),
        "the name spilled onto the row beneath it: {:?}",
        region(selected + 1)
    );

    let line = row_text(&screen, selected);
    let divider = line
        .char_indices()
        .filter(|(_, character)| *character == '│')
        .nth(1)
        .map(|(byte_index, _)| u16::try_from(line[..byte_index].chars().count()).expect("column"))
        .expect("variants region divider");
    assert_eq!(
        screen[(divider - 1, selected)].style().bg,
        Some(SURFACE_3),
        "the band should reach the end of the pane: {line:?}"
    );
}

/// The aside takes its full share at 151 columns. The Sources panes are
/// bounded so that crossing it costs them slack and nothing else: every row
/// reads the same on both sides.
#[test]
fn crossing_the_wide_detail_threshold_never_shrinks_the_sources_panes() {
    let harness = Harness::new();
    let repository = harness
        .directory
        .path()
        .join("source-checkout-with-a-long-directory-name");
    // Both bounds have to bind for the comparison to mean anything: a catalog
    // path and a variant name short enough to fit the pane on either side of
    // the crossing would read the same however the panes were laid out.
    let catalog = "experimental/nested/claude-code/skills";
    let variant = "portable-variant-with-a-very-long-and-descriptive-directory-name";
    let directory = repository.join(catalog).join(variant);
    fs::create_dir_all(&directory).expect("create crossing fixture");
    fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {variant}\ndescription: Crossing fixture\n---\n# Fixture\n"),
    )
    .expect("write crossing fixture");
    create_repository(&repository);
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    app.update(Action::OpenSources);

    // The content of one region on the row holding `needle`, read between the
    // rules that divide the regions.
    let region = |width: u16, index: usize, needle: &str| {
        let screen = buffer(&app, width, 40);
        let line = row_text(&screen, row_containing(&screen, needle));
        let dividers = line
            .char_indices()
            .filter(|(_, character)| *character == '│')
            .map(|(byte_index, _)| byte_index)
            .collect::<Vec<_>>();
        let start = if index == 0 {
            0
        } else {
            dividers[index - 1] + '│'.len_utf8()
        };
        line[start..dividers[index]].trim_end().to_owned()
    };

    for (index, needle) in [
        (0, "source-checkout"),
        (0, "@"),
        (1, "experimental/nested/claude-code/skills"),
        (1, "✓ valid"),
    ] {
        assert_eq!(
            region(150, index, needle),
            region(151, index, needle),
            "the Sources panes gave up columns to the aside at the crossing"
        );
    }

    // Bounding the content leaves slack on a very wide terminal, and the
    // selected row's band crosses it: a band stopping where the name does
    // would read as a row ending mid-region.
    const SURFACE_3: Color = Color::Rgb(0x17, 0x21, 0x2c);
    let screen = buffer(&app, 200, 40);
    let selected = row_containing(&screen, "▌ ✓ valid");
    let line = row_text(&screen, selected);
    let divider_index = line
        .char_indices()
        .filter(|(_, character)| *character == '│')
        .nth(1)
        .map(|(byte_index, _)| byte_index)
        .expect("variants region divider");
    let divider = u16::try_from(line[..divider_index].chars().count()).expect("column");
    // The name is capped, so the row's content ends well before the pane does.
    let content = u16::try_from(line[..divider_index].trim_end().chars().count()).expect("column");
    assert!(
        divider > content + 10,
        "expected slack beside the bounded pane: {line:?}"
    );
    assert_eq!(
        screen[(divider - 1, selected)].style().bg,
        Some(SURFACE_3),
        "the band should cross the slack, not stop at the name"
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

/// A source with more empty catalogs than the pane has rows keeps every one
/// of them reachable: the catalog-state rows are the pane's rows when no
/// candidates exist, so focus moves over them and the window follows, instead
/// of an unwindowed list clipping the tail with nothing to scroll it by.
#[test]
fn every_catalog_of_an_all_empty_source_is_reachable_by_moving_the_selection() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    fs::create_dir_all(repository.join("skills")).expect("create common catalog");
    fs::write(repository.join("skills/.keep"), "empty catalog fixture").expect("write keep file");
    let names: Vec<String> = (1..=11).map(|index| format!("c{index:02}")).collect();
    for name in &names {
        let catalog = repository.join(name).join("claude-code/skills");
        fs::create_dir_all(&catalog).expect("create agent catalog");
        fs::write(catalog.join(".keep"), "empty catalog fixture").expect("write keep file");
    }
    create_repository(&repository);
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    app.update(Action::OpenSources);
    app.update(Action::AdvanceSourcesPane);

    let mut labels: Vec<String> = names
        .iter()
        .map(|name| format!("{name}/claude-code/skills"))
        .collect();
    // Anchored to the start of a row, because `skills` on its own is a
    // substring of every `cNN/claude-code/skills` label and would be seen on
    // the first frame. Anchoring on the qualifiers instead would make this
    // test report an unreachable catalog if they were ever shed here.
    labels.push("\nskills".to_owned());
    let catalog_count = labels.len();

    // Twelve catalogs need twenty-four rows, so the first render cannot hold
    // them all — otherwise this test would pass without any windowing.
    let rendered = text(&buffer(&app, 80, 24));
    assert!(
        labels
            .iter()
            .any(|label| !rendered.contains(label.as_str())),
        "every catalog label fit on one screen, so nothing is clipped:\n{rendered}"
    );
    assert!(rendered.contains("▌ no variants"), "{rendered}");
    // The rows are movable, so the key-hint bar must say so: reaching the
    // clipped catalogs is only real if the way there is advertised.
    assert!(rendered.contains("j/k Move"), "{rendered}");

    // Walking the selection across every catalog brings each label into view.
    let mut seen: Vec<bool> = vec![false; catalog_count];
    for _ in 0..catalog_count {
        let rendered = text(&buffer(&app, 80, 24));
        assert!(rendered.contains("▌ no variants"), "{rendered}");
        for (index, label) in labels.iter().enumerate() {
            seen[index] |= rendered.contains(label.as_str());
        }
        app.update(Action::MoveSourcesSelection(1));
    }
    for (index, label) in labels.iter().enumerate() {
        assert!(
            seen[index],
            "catalog {label:?} was never scrolled into view"
        );
    }
}

/// One candidate somewhere must not strand the other catalogs: every rendered
/// row is a focus position — each candidate, and each catalog's state row —
/// so a source mixing one skill with many empty catalogs can still be walked
/// to its end.
#[test]
fn a_source_with_one_candidate_and_many_empty_catalogs_is_still_walkable() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    fs::create_dir_all(repository.join("skills/portable")).expect("create candidate");
    fs::write(
        repository.join("skills/portable/SKILL.md"),
        "---\nname: portable\ndescription: Portable fixture\n---\n# Portable\n",
    )
    .expect("write candidate");
    let names: Vec<String> = (1..=11).map(|index| format!("c{index:02}")).collect();
    for name in &names {
        let catalog = repository.join(name).join("claude-code/skills");
        fs::create_dir_all(&catalog).expect("create agent catalog");
        fs::write(catalog.join(".keep"), "empty catalog fixture").expect("write keep file");
    }
    create_repository(&repository);
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    app.update(Action::OpenSources);
    app.update(Action::AdvanceSourcesPane);

    let labels: Vec<String> = names
        .iter()
        .map(|name| format!("{name}/claude-code/skills"))
        .collect();
    let row_count = labels.len() + 1;

    let rendered = text(&buffer(&app, 80, 24));
    assert!(
        labels
            .iter()
            .any(|label| !rendered.contains(label.as_str())),
        "every catalog label fit on one screen, so nothing is clipped:\n{rendered}"
    );

    let mut seen: Vec<bool> = vec![false; labels.len()];
    for _ in 0..row_count {
        let rendered = text(&buffer(&app, 80, 24));
        for (index, label) in labels.iter().enumerate() {
            seen[index] |= rendered.contains(label.as_str());
        }
        app.update(Action::MoveSourcesSelection(1));
    }
    for (index, label) in labels.iter().enumerate() {
        assert!(
            seen[index],
            "catalog {label:?} was never scrolled into view"
        );
    }
}

/// A source that could not be read at all renders no rows, so it counts
/// none: the `j/k Move` hint must not appear for an unavailable source even
/// when several catalog roots are registered, because pressing the key would
/// walk a selection nothing on screen shows.
#[test]
fn an_unavailable_source_offers_no_selection_to_move() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    let moved = harness.directory.path().join("source-moved");
    create_two_catalog_source_fixture(&repository);
    let environment = harness.environment();
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    drop(app);
    fs::rename(&repository, &moved).expect("move registered checkout");
    let mut reopened = SkilledApp::open(environment).expect("reopen application");
    reopened.update(Action::OpenSources);
    reopened.update(Action::AdvanceSourcesPane);

    let before = buffer(&reopened, 80, 24);
    assert!(text(&before).contains("× unavailable"), "{}", text(&before));
    assert!(
        !text(&before).contains("j/k Move"),
        "the hint offers a move the pane cannot show:\n{}",
        text(&before)
    );

    reopened.update(Action::MoveSourcesSelection(1));
    let after = buffer(&reopened, 80, 24);
    assert_eq!(
        text(&before),
        text(&after),
        "moving the selection changed a pane that renders no rows"
    );
}

/// A catalog's error row is a focus position like any other row, so its text
/// is bounded to the pane the way a variant name is: a wrapped error would
/// put the marker and the band on one line and the words on the next.
#[test]
fn a_selected_catalog_error_row_is_bounded_and_banded_like_any_row() {
    const SURFACE_3: Color = Color::Rgb(0x17, 0x21, 0x2c);

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
    create_repository(&repository);
    let environment = harness.environment();
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    drop(app);
    let connection =
        rusqlite::Connection::open(harness.directory.path().join("data/skilled.sqlite3"))
            .expect("open application database");
    connection
        .execute(
            "UPDATE catalog_roots SET relative_path = '../outside' \
             WHERE relative_path LIKE 'catalog-b/%'",
            [],
        )
        .expect("corrupt one stored catalog path");
    drop(connection);
    let mut reopened = SkilledApp::open(environment).expect("reopen application");
    reopened.update(Action::OpenSources);
    reopened.update(Action::AdvanceSourcesPane);

    // The corrupted catalog sorts first, so its error row is the selection.
    let screen = buffer(&reopened, 100, 40);
    let selected = row_containing(&screen, "▌ × unavailable");
    let region = |y: u16| {
        let line = row_text(&screen, y);
        let dividers = line
            .char_indices()
            .filter(|(_, character)| *character == '│')
            .map(|(byte_index, _)| byte_index)
            .collect::<Vec<_>>();
        line[dividers[0] + '│'.len_utf8()..dividers[1]].to_owned()
    };
    // Bounded, with the ellipsis saying there was more; the rest of the
    // message is given in the detail region.
    assert!(
        region(selected).trim_end().ends_with("..."),
        "{:?}",
        region(selected)
    );
    // The next region row is the healthy catalog's label, not a wrapped
    // continuation of the error.
    assert!(
        region(selected + 1).trim_start().starts_with("catalog-a"),
        "{:?}",
        region(selected + 1)
    );
    assert_eq!(
        style_in_row(&screen, selected, "× unavailable").bg,
        Some(SURFACE_3)
    );
}

/// The selection the pane shows carries across the region boundary: a focused
/// catalog-state row selects its catalog, so the Details CATALOG section
/// names the catalog the band is on rather than rendering identically for
/// every position.
#[test]
fn focusing_an_empty_catalog_row_selects_that_catalog_in_details() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    fs::create_dir_all(repository.join("skills/portable")).expect("create candidate");
    fs::write(
        repository.join("skills/portable/SKILL.md"),
        "---\nname: portable\ndescription: Portable fixture\n---\n# Portable\n",
    )
    .expect("write candidate");
    let empty = repository.join("experimental/claude-code/skills");
    fs::create_dir_all(&empty).expect("create empty catalog");
    fs::write(empty.join(".keep"), "empty catalog fixture").expect("write keep file");
    create_repository(&repository);
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    app.update(Action::OpenSources);
    app.update(Action::AdvanceSourcesPane);

    // Walk the selection until the band rests on the empty catalog's state
    // row — bounded, so a selection that cannot reach it fails rather than
    // spins — then open Details.
    for _ in 0..4 {
        if text(&buffer(&app, 80, 24)).contains("▌ no variants") {
            break;
        }
        app.update(Action::MoveSourcesSelection(1));
    }
    assert!(
        text(&buffer(&app, 80, 24)).contains("▌ no variants"),
        "the selection never reached the empty catalog's state row"
    );
    app.update(Action::AdvanceSourcesPane);

    let details = text(&buffer(&app, 80, 24));
    assert!(
        details.contains("Path: experimental/claude-code/skills"),
        "{details}"
    );
    assert!(!details.contains("Directory: portable"), "{details}");
}

/// The generic empty state is reserved for a source with no catalogs at all.
/// A catalog that scanned clean but holds nothing is still named, with the
/// `no variants` line beneath its label: flattening it into the empty state
/// would hide which catalogs the source has and that each was read.
#[test]
fn an_emptied_catalog_is_named_rather_than_flattened_into_the_empty_state() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    create_source_fixture(&repository);
    let environment = harness.environment();
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    drop(app);
    fs::remove_dir_all(repository.join("skills/portable")).expect("empty the catalog");
    let mut reopened = SkilledApp::open(environment).expect("reopen application");
    reopened.update(Action::OpenSources);
    reopened.update(Action::AdvanceSourcesPane);

    let screen = buffer(&reopened, 80, 24);
    let rendered = text(&screen);
    let label = row_text(&screen, row_containing(&screen, "Common · all agents"));
    assert!(label.starts_with("skills  "), "{label:?}");
    assert!(rendered.contains("no variants"), "{rendered}");
    assert!(!rendered.contains("No variants found"), "{rendered}");
    // The hint belongs to catalog errors, and this catalog read cleanly.
    assert!(!rendered.contains("Open Details"), "{rendered}");
    assert!(rendered.contains("0 found"), "{rendered}");
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
    // The failed catalog sorts first and its error row is a focus position,
    // so the healthy variant is one move down.
    reopened.update(Action::MoveSourcesSelection(1));
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
    let short_head = app.sources()[0].short_head().to_owned();
    let canonical = repository.canonicalize().expect("canonical repository");
    app.update(Action::OpenSources);
    app.update(Action::AdvanceSourcesPane);
    app.update(Action::AdvanceSourcesPane);

    let screen = buffer(&app, 80, 24);
    let rendered = text(&screen);

    for expected in [
        "REPOSITORY",
        "Label: source",
        "Branch: main",
        "Status: ✓ clean",
        "Remote: https://example.test/source.git",
        "Last scan:",
        "CATALOG",
        "Classification: Common",
        "Registered for: all agents",
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
    assert!(rendered.contains(&short_head), "{rendered}");
    // The checkout is named as far as the region allows: a path too long for
    // one line is cut in its middle, so what is shown is still this path's
    // beginning and this path's end.
    let canonical = canonical.display().to_string();
    let field = rendered
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("Path: /"))
        .map(|value| format!("/{}", value.trim_end()))
        .unwrap_or_else(|| panic!("no repository path field in\n{rendered}"));
    let (start, end) = field.split_once("...").unwrap_or((field.as_str(), ""));
    assert!(
        canonical.starts_with(start) && canonical.ends_with(end),
        "{field:?} should stand for {canonical:?}"
    );
    // The shared section helper restyled these kickers with the Inventory
    // ones; the words alone would not notice a colour regression here.
    const MUTED: Color = Color::Rgb(0x84, 0x91, 0xa1);
    for heading in ["REPOSITORY", "CATALOG", "VARIANT"] {
        assert_eq!(
            style_in_row(&screen, row_containing(&screen, heading), heading).fg,
            Some(MUTED),
            "{heading} should be a muted kicker"
        );
    }
}

/// A path has no spaces to wrap at, so a path too long for the region used to
/// break mid-word and continue on the next line, which reads as two entries
/// and can be cut from its label by the row budget. Every path field is one
/// line, middle-truncated, so both ends of it survive.
#[test]
fn sources_details_keep_every_path_on_its_own_line() {
    let harness = Harness::new();
    let repository = harness
        .directory
        .path()
        .join("a-deliberately-long-checkout-directory")
        .join("nested-below-another-long-directory");
    let catalog = "deeply/nested/experimental/claude-code/skills";
    let variant = repository.join(catalog).join("experimental");
    fs::create_dir_all(&variant).expect("create catalog fixture");
    fs::write(
        variant.join("SKILL.md"),
        "---\nname: experimental\ndescription: Experimental fixture\n---\n# Fixture\n",
    )
    .expect("write catalog fixture");
    create_repository(&repository);
    git(
        &repository,
        &[
            "remote",
            "add",
            "origin",
            &format!(
                "https://example.test/{}source.git",
                "remote-segment/".repeat(8)
            ),
        ],
    );
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    app.update(Action::OpenSources);
    app.update(Action::AdvanceSourcesPane);
    app.update(Action::AdvanceSourcesPane);

    // The compact drill-in and the aside beside the panes, since the aside is
    // the narrower of the two and the one a long path crowds first.
    for (width, height) in [(80, 24), (120, 40), (160, 48)] {
        let region = detail_region_lines(&text(&buffer(&app, width, height)));
        // Anchored on the section a field belongs to: REPOSITORY, CATALOG and
        // VARIANT each state a `Path`, and which one is meant should not rest
        // on the order the sections happen to be rendered in.
        let field = |section: &str, label: &str| {
            let start = region
                .iter()
                .position(|line| line == section)
                .unwrap_or_else(|| panic!("{section:?} missing at {width} columns\n{region:#?}"));
            let index = region[start..]
                .iter()
                .position(|line| line.starts_with(label))
                .map(|offset| start + offset)
                .unwrap_or_else(|| {
                    panic!("{label:?} missing under {section} at {width} columns\n{region:#?}")
                });
            let next = region.get(index + 1).cloned().unwrap_or_else(|| {
                panic!("{label:?} ends the region at {width} columns\n{region:#?}")
            });
            (region[index].clone(), next)
        };

        // Each path field is followed by the next field, so none of them spill
        // a second line of path into the region.
        let (path, next) = field("REPOSITORY", "Path: ");
        assert!(path.contains("..."), "{path:?} at {width} columns");
        assert!(
            path.ends_with("directory"),
            "the end of the checkout should survive: {path:?} at {width} columns"
        );
        assert!(next.starts_with("HEAD: "), "{next:?} at {width} columns");

        let (remote, next) = field("REPOSITORY", "Remote: ");
        assert!(remote.contains("..."), "{remote:?} at {width} columns");
        assert!(
            remote.ends_with("source.git"),
            "{remote:?} at {width} columns"
        );
        assert!(next.starts_with("Status: "), "{next:?} at {width} columns");

        // The catalog path is one line the same way, and the classification it
        // is stated with stays whole whether it shares that line or takes its
        // own — it is never cut, and never crowds the path into an elision.
        let (catalog_path, next) = field("CATALOG", "Path: ");
        let shown = catalog_path
            .strip_prefix("Path: ")
            .and_then(|value| value.split(" · ").next())
            .unwrap_or_default();
        assert!(
            stands_for(shown, catalog),
            "{shown:?} should stand for {catalog:?} at {width} columns"
        );
        assert!(
            catalog_path.ends_with(" · Classification: Agent-specific")
                || next == "Classification: Agent-specific",
            "the classification should be stated whole: {catalog_path:?} then {next:?} at {width} columns"
        );
    }
}

/// Whether a shown value is `whole` cut in the middle: what is left of the
/// ellipsis begins it, and what is right of the ellipsis ends it.
fn stands_for(shown: &str, whole: &str) -> bool {
    let (start, end) = shown.split_once("...").unwrap_or((shown, ""));
    whole.starts_with(start) && whole.ends_with(end)
}

/// The detail region's text, with the panes it sits beside stripped away: the
/// region is the last column of the screen, so whatever follows the rightmost
/// vertical rule is its own.
fn detail_region_lines(rendered: &str) -> Vec<String> {
    rendered
        .lines()
        .map(|line| {
            line.rsplit_once('│')
                .map_or(line, |(_, region)| region)
                .trim()
                .to_owned()
        })
        .collect()
}

/// The detail pane and the variants group label answer the same question, so
/// they answer it in the same words: the agents the catalog is registered for,
/// named. A catalog claiming one agent says that agent and stops — the list is
/// the claim, and the agents it leaves out are the ones not claimed.
#[test]
fn sources_details_name_the_agents_a_catalog_claims_in_the_group_label_words() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    create_two_catalog_source_fixture(&repository);
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    app.update(Action::OpenSources);
    // Wide enough for the detail region to sit beside the variants, so the
    // selection can be moved and the claim read in one screen.
    app.update(Action::AdvanceSourcesPane);

    let claim = |app: &SkilledApp| {
        let rendered = text(&buffer(app, 120, 40));
        assert!(
            !rendered.contains("Codex: no") && !rendered.contains("Codex: yes"),
            "the detail pane should not keep the yes/no vocabulary\n{rendered}"
        );
        rendered
            .lines()
            .find_map(|line| line.split_once("Registered for:"))
            .map(|(_, claim)| format!("Registered for:{}", claim.trim_end()))
            .unwrap_or_else(|| String::from("<no registration line>"))
    };

    // The agent-specific catalog claims the one agent its path names.
    assert_eq!(claim(&app), "Registered for: Claude Code");

    // Moving into the common catalog changes the claim with the selection. The
    // walk is asserted to have arrived: a claim read off a selection that never
    // moved would be the first catalog's answer to a question about the second.
    let common_catalog_selected =
        |app: &SkilledApp| text(&buffer(app, 120, 40)).contains("Classification: Common");
    let mut selected = common_catalog_selected(&app);
    for _ in 0..4 {
        if selected {
            break;
        }
        app.update(Action::MoveSourcesSelection(1));
        selected = common_catalog_selected(&app);
    }
    assert!(
        selected,
        "the selection should reach the common catalog\n{}",
        text(&buffer(&app, 120, 40))
    );
    assert_eq!(claim(&app), "Registered for: all agents");
}

/// The Sources detail region reports a cut in words and in a tone, the way the
/// Inventory region does — colour alone is not a signal a terminal can be
/// relied on to carry, and a description that simply stops reads as a
/// description that ended.
#[test]
fn a_truncated_sources_detail_region_reports_the_cut() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    let variant = repository.join("skills/verbose");
    fs::create_dir_all(&variant).expect("create catalog fixture");
    fs::write(
        variant.join("SKILL.md"),
        "---\nname: verbose\ndescription: A description long enough to outgrow the detail \
         region at twenty-four rows, so the region has to say what it could not show \
         rather than ending in the middle of this sentence.\n---\n# Verbose\n",
    )
    .expect("write catalog fixture");
    create_repository(&repository);
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    app.update(Action::OpenSources);

    let screen = buffer(&app, 120, 24);
    let notice = row_containing(&screen, "more line");
    let line = row_text(&screen, notice);
    assert!(line.contains("! "), "{line}");
    assert_eq!(
        style_in_row(&screen, notice, "!").fg,
        Some(Color::Rgb(0xe6, 0xbd, 0x6a))
    );

    // A region tall enough for every section says nothing: the notice is a
    // report of a cut, not a permanent fixture of the screen.
    let whole = text(&buffer(&app, 120, 60));
    assert!(!whole.contains("more line"), "{whole}");
    assert!(whole.contains("this sentence."), "{whole}");

    // The count is a measurement, so it is checked against the screen rather
    // than against itself: at every height the region can be cut at, what it
    // says it dropped is what a region tall enough to hold everything shows
    // and this one does not. A stated count that drifts from the rows on
    // screen is worse than no notice, because it is read as a fact.
    let detail_rows = |height: u16| {
        text(&buffer(&app, 120, height))
            .lines()
            .filter_map(|line| line.rsplit_once('│'))
            .map(|(_, region)| region.trim().to_owned())
            .filter(|region| !region.is_empty())
            .collect::<Vec<_>>()
    };
    let whole_rows = detail_rows(60).len();
    let mut cut_heights = 0;
    for height in 24..40 {
        let rows = detail_rows(height);
        let stated = rows
            .iter()
            .find_map(|row| row.split_once(" more line"))
            .and_then(|(count, _)| count.trim_start_matches("! ").parse::<usize>().ok());
        match stated {
            // The notice's own row is not content the region showed.
            Some(stated) => {
                cut_heights += 1;
                assert_eq!(
                    stated,
                    whole_rows - (rows.len() - 1),
                    "at height {height} the region showed {} of {whole_rows} rows",
                    rows.len() - 1
                );
            }
            // Silence is a claim too: it says everything is here.
            None => assert_eq!(
                rows.len(),
                whole_rows,
                "at height {height} the region dropped rows without saying so"
            ),
        }
    }
    assert!(
        cut_heights > 0,
        "the sweep should reach heights where the region is cut"
    );
}

/// The longest claim two agents can make outruns the narrowest detail region's
/// line by a column, so it wraps onto a second one. This pins that the wrap is
/// all that happens to it: both agents are still named, in order, and the
/// continuation is not cut away by the section's row budget — an elided claim
/// would name a registration the user never confirmed.
#[test]
fn the_longest_two_agent_registration_claim_survives_the_narrowest_detail_region() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    create_source_fixture(&repository);
    let mut app = harness.completed_setup();
    app.update(Action::OpenSources);
    app.update(Action::BeginAddSource);
    for character in repository.to_string_lossy().chars() {
        app.update(Action::AppendSourcePath(character));
    }
    let update = app.update(Action::SubmitSourcePath);
    app.perform_effects(update.effects())
        .expect("inspect source");
    // The common catalog claims every agent by default; dropping Codex leaves
    // the two whose names are longest.
    app.update(Action::ToggleCatalogCompatibility(AgentKind::Codex));
    let update = app.update(Action::ConfirmPendingSource);
    app.perform_effects(update.effects())
        .expect("register source");
    app.update(Action::OpenSources);
    app.update(Action::AdvanceSourcesPane);

    // 120 columns is the narrow aside: wide enough for the detail region to
    // sit beside the variants, narrow enough that it takes its lesser share.
    let rendered = text(&buffer(&app, 120, 40));
    let stated = rendered
        .lines()
        .filter_map(|line| line.rsplit_once('│'))
        .map(|(_, region)| region.trim_end())
        .skip_while(|region| !region.trim_start().starts_with("Registered for:"))
        .take(2)
        .map(|region| region.trim().to_owned())
        .collect::<Vec<_>>()
        .join(" ");

    assert_eq!(
        stated, "Registered for: Claude Code + OpenCode",
        "the claim should wrap whole rather than elide\n{rendered}"
    );
}

/// A forty-character revision does not fit the detail region at any supported
/// width: wrapped, the row budget could cut the line between `HEAD:` and its
/// value and leave the field saying nothing at all, which is what happened at
/// 100 to 150 columns. The abbreviation Git itself prints fits on the label's
/// own line at every width.
#[test]
fn sources_details_state_the_revision_in_the_abbreviated_form_at_every_width() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    create_source_fixture(&repository);
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    let head = app.sources()[0].head().to_owned();
    let short_head = app.sources()[0].short_head().to_owned();
    app.update(Action::OpenSources);
    app.update(Action::AdvanceSourcesPane);
    app.update(Action::AdvanceSourcesPane);

    for width in [80, 100, 110, 120, 140, 150, 200] {
        let rendered = text(&buffer(&app, width, 40));
        assert!(
            rendered.contains(&format!("HEAD: {short_head}")),
            "the revision is missing at {width} columns\n{rendered}"
        );
        assert!(
            !rendered.contains(&head),
            "the whole revision should never be shown at {width} columns\n{rendered}"
        );
    }
}

/// The stored scan time is seconds since the epoch. Rendered as those digits
/// it tells the reader nothing, so the pane states the civil date it stands
/// for and names the zone it is in.
///
/// The zone has to reach the reader on the label's own line at every width.
/// A timestamp wrapped away from `Last scan:` leaves the label saying nothing,
/// which is the bug the abbreviated revision fixed; one wrapped after its last
/// space is worse than that, because `2026-08-05 04:14` reads as a complete
/// time and the row that would have said which zone it is in is somewhere
/// else. Both detail-region width tiers are swept, either side of
/// `DETAIL_REGION_WIDE_THRESHOLD`.
#[test]
fn sources_details_state_the_last_scan_as_a_date_rather_than_an_epoch() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    create_source_fixture(&repository);
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    let stored = app.sources()[0].last_scan_at();
    app.update(Action::OpenSources);
    app.update(Action::AdvanceSourcesPane);
    app.update(Action::AdvanceSourcesPane);

    let mut scanned = String::new();
    for width in [80, 100, 120, 150, 151, 160, 200] {
        let rendered = text(&buffer(&app, width, 40));
        assert!(
            !rendered.contains(&stored.to_string()),
            "the raw epoch should not be shown at {width} columns\n{rendered}"
        );
        // Read from the label's own row and no further, so a timestamp that
        // wrapped onto the row below is a missing one.
        scanned = rendered
            .lines()
            .find_map(|line| line.split_once("Last scan: "))
            .map(|(_, rest)| rest.trim_end().to_owned())
            .unwrap_or_else(|| String::from("<no last scan line>"));
        assert!(
            is_utc_minute_timestamp(&scanned),
            "{scanned:?} should read as a whole UTC timestamp at {width} columns\n{rendered}"
        );
        // The drill-in states the scan time beside the status, since the row it
        // would otherwise spend is one the sections below it need; the aside is
        // too narrow for both, so there the scan time takes its own row. The
        // shared line is 49 cells at its shortest — `Status: `, the briefest
        // badge, ` · `, `Last scan: `, and the twenty of the timestamp — which
        // the drill-in has and neither aside tier does at 37 or 47. So 100 here
        // is the width the aside first appears at and not a width of its own:
        // the region asks whether the line fits, never how wide the terminal is.
        let shared = rendered
            .lines()
            .any(|line| line.contains("Status: ") && line.contains("Last scan: "));
        assert_eq!(
            shared,
            width < 100,
            "the scan time shares the status row only where both fit at {width} columns\n{rendered}"
        );
    }

    // The formatter's own cases are unit-tested; here the point is that the
    // moment shown is the one that was stored. Read back independently of the
    // rendering, the date and time must land on the stored second, give or
    // take the minute it is truncated to.
    let elapsed = stored - epoch_seconds_of(&scanned);
    assert!(
        (0..60).contains(&elapsed),
        "{scanned:?} should stand for the stored scan time"
    );
}

/// The epoch second a `YYYY-MM-DD HH:MM UTC` string names, by day counting
/// rather than by the formatter's own arithmetic.
fn epoch_seconds_of(timestamp: &str) -> i64 {
    let number = |range: std::ops::Range<usize>| {
        timestamp[range]
            .parse::<i64>()
            .unwrap_or_else(|_| panic!("{timestamp:?} should be a timestamp"))
    };
    let (year, month, day) = (number(0..4), number(5..7), number(8..10));
    // Counting forward from 1970 only answers for years at or after it; a
    // caller handed an earlier one would otherwise be given a confident zero.
    assert!(year >= 1970, "{timestamp:?} is before the epoch");
    let leap = |year: i64| year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let mut days = (1970..year)
        .map(|year| if leap(year) { 366 } else { 365 })
        .sum::<i64>();
    let lengths = [
        31,
        if leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    days += lengths[..usize::try_from(month).expect("month") - 1]
        .iter()
        .sum::<i64>()
        + day
        - 1;
    days * 86_400 + number(11..13) * 3_600 + number(14..16) * 60
}

/// `YYYY-MM-DD HH:MM UTC` and nothing else.
fn is_utc_minute_timestamp(value: &str) -> bool {
    let shape = "dddd-dd-dd dd:dd UTC";
    value.len() == shape.len()
        && value.chars().zip(shape.chars()).all(|(actual, expected)| {
            if expected == 'd' {
                actual.is_ascii_digit()
            } else {
                actual == expected
            }
        })
}

/// The region says nothing when it runs out of rows: a section cut short reads
/// as a section that had no more to say. So the densest thing it can be asked
/// to state — a catalog path long enough to take its classification onto a
/// second line, and an invalid variant whose validation error wraps onto a
/// second line of its own — has to reach its last line at the supported
/// minimum, where the drill-in has the whole screen and no aside to spill into.
/// Every row spent in the drill-in is measured against that.
///
/// The claim is that size and no other. A terminal 24 rows tall but wide enough
/// for the aside gives this same fixture fewer rows than it needs, and the
/// region ends mid-sentence without saying so, where the Inventory region would
/// state what it dropped. That is older than this test and outside the issue it
/// was written for; it is recorded here so the coverage is not read as wider
/// than it is.
#[test]
fn sources_details_state_their_last_line_at_the_minimum_supported_size() {
    let harness = Harness::new();
    let repository = harness.directory.path().join("source");
    let catalog = repository.join("deeply/nested/experimental/claude-code/skills");
    fs::create_dir_all(catalog.join("experimental")).expect("create catalog fixture");
    fs::write(
        catalog.join("experimental").join("SKILL.md"),
        "---\nname: experimental\ndescription: Experimental fixture\n---\n# Fixture\n",
    )
    .expect("write catalog fixture");
    fs::create_dir_all(catalog.join("broken")).expect("create invalid variant");
    fs::write(catalog.join("broken").join("skill.md"), "wrong name\n")
        .expect("write invalid variant");
    create_repository(&repository);
    let mut app = harness.completed_setup();
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    app.update(Action::OpenSources);
    app.update(Action::AdvanceSourcesPane);
    app.update(Action::AdvanceSourcesPane);

    let rendered = text(&buffer(&app, 80, 24));

    for expected in [
        "Classification: Agent-specific",
        "Registered for: Claude Code",
        "Directory: broken · Name: broken",
        "Status: × invalid",
        // The last line of the last section: the tail of the wrapped
        // validation error, and the first thing a region one row short of its
        // content drops.
        "SKILL.md",
    ] {
        assert!(
            rendered.contains(expected),
            "missing {expected:?} in\n{rendered}"
        );
    }
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
    // The remote is bounded rather than given whole, so it cannot crowd the
    // fields below it off the region. It is cut in the middle and on one line,
    // so the row it is on still ends the way the remote itself does.
    assert!(
        rendered.lines().any(|line| line.contains("remote-segment")
            && line.contains("...")
            && line.trim_end().ends_with("remote-segment/")),
        "{rendered}"
    );
    assert!(
        !rendered.contains(&"remote-segment/".repeat(16)),
        "{rendered}"
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
    let focused = row_starting_with(&screen, "▌ second");
    let unfocused = row_starting_with(&screen, "  first");

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
        create_repository(&repository);

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

    /// Three rows whose verdict comes from OpenCode's effective resolution
    /// rather than from any one installation: one name resolving to two
    /// different directories, one Claude Code edition reaching OpenCode
    /// through a compatibility root, and one common variant whose catalog
    /// explicitly excludes OpenCode.
    #[cfg(unix)]
    fn resolution_inventory(&self) -> SkilledApp {
        let home = self.directory.path().join("home");
        let first = home.join("alpha");
        write_skill_fixture(&first.join("skills/review"), "review");
        write_skill_fixture(&first.join("claude/skills/exposed"), "exposed");
        create_repository(&first);
        let second = home.join("beta");
        write_skill_fixture(&second.join("skills/review"), "review");
        create_repository(&second);
        let third = home.join("gamma");
        write_skill_fixture(&third.join("skills/excluded"), "excluded");
        create_repository(&third);

        let mut app = self.completed_setup();
        for repository in [&first, &second] {
            let preview = app.preview_source(repository).expect("preview source");
            app.confirm_source(preview).expect("register source");
        }
        app.update(Action::OpenSources);
        app.update(Action::BeginAddSource);
        for character in third.to_string_lossy().chars() {
            app.update(Action::AppendSourcePath(character));
        }
        let update = app.update(Action::SubmitSourcePath);
        app.perform_effects(update.effects())
            .expect("inspect source");
        app.update(Action::ToggleCatalogCompatibility(AgentKind::OpenCode));
        let update = app.update(Action::ConfirmPendingSource);
        app.perform_effects(update.effects())
            .expect("register source");
        let claude = home.join(".claude/skills");
        let opencode = home.join(".config/opencode/skills");
        fs::create_dir_all(&claude).expect("create Claude Code root");
        fs::create_dir_all(&opencode).expect("create OpenCode root");
        fs::create_dir_all(home.join(".agents/skills")).expect("create Codex root");
        symlink(first.join("skills/review"), opencode.join("review"));
        symlink(second.join("skills/review"), claude.join("review"));
        symlink(first.join("claude/skills/exposed"), claude.join("exposed"));
        symlink(third.join("skills/excluded"), claude.join("excluded"));

        app.update(Action::OpenSources);
        let update = app.update(Action::OpenInventory);
        app.perform_effects(update.effects())
            .expect("installation scan");
        app
    }

    /// One `alpha` skill linked into all three agent roots from the
    /// registered checkout.
    ///
    /// Its detail region carries a section for every agent, which is more
    /// than the minimum supported terminal can hold at once — the arrangement
    /// the issue behind the scrollable region names.
    #[cfg(unix)]
    fn everywhere_installed_inventory(&self) -> SkilledApp {
        let home = self.directory.path().join("home");
        // Named long enough that the observed target path cannot share a row
        // with its label: the field then wraps, and the window has a line
        // whose first row states a path as empty if it is cut after it.
        let repository = home.join("library-checked-out-under-a-long-name");
        let skill = repository.join("skills/alpha");
        fs::create_dir_all(&skill).expect("create skill fixture");
        // The description is long enough to wrap in a detail region at any
        // width, so the region holds a line worth more than one row.
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: alpha\ndescription: A description long enough to wrap onto \
             several rows of the detail region at every width a supported terminal \
             gives it, so a window counted in rows is not the same as one counted \
             in lines.\n---\n# alpha\n",
        )
        .expect("write skill fixture");
        create_repository(&repository);

        let mut app = self.completed_setup();
        let preview = app.preview_source(&repository).expect("preview source");
        app.confirm_source(preview).expect("register source");

        for root in [
            ".claude/skills",
            ".agents/skills",
            ".config/opencode/skills",
        ] {
            let root = home.join(root);
            fs::create_dir_all(&root).expect("create agent skill root");
            symlink(repository.join("skills/alpha"), root.join("alpha"));
        }

        app.update(Action::OpenSources);
        let update = app.update(Action::OpenInventory);
        app.perform_effects(update.effects())
            .expect("installation scan");
        app
    }

    /// One `gamma` skill installed two ways: linked from the registered
    /// checkout for Claude Code, and copied outright for Codex.
    ///
    /// The link resolves to a registered source and the copy resolves to
    /// none, which is the arrangement no single source describes.
    #[cfg(unix)]
    fn mixed_provenance_inventory(&self) -> SkilledApp {
        let mut app = self.gamma_installed_two_ways();
        scan_installations(&mut app);
        app
    }

    /// The same two installations, with the checkout moved away between
    /// registration and the scan.
    ///
    /// No registered source can then be accounted for, so an installation
    /// that resolves to none is undetermined rather than unregistered.
    #[cfg(unix)]
    fn unverified_provenance_inventory(&self) -> SkilledApp {
        let mut app = self.gamma_installed_two_ways();
        let home = self.directory.path().join("home");
        fs::rename(home.join("library"), home.join("moved-away")).expect("move the checkout away");
        scan_installations(&mut app);
        app
    }

    /// One `gamma` skill linked from each of two registered checkouts, so
    /// both installations resolve but to different sources.
    ///
    /// Naming either one would misstate the other, which is what the row
    /// reports instead.
    #[cfg(unix)]
    fn divergent_provenance_inventory(&self) -> SkilledApp {
        let home = self.directory.path().join("home");
        let mut app = self.completed_setup();
        let library = self.registered_gamma_source(&mut app, "library");
        let annex = self.registered_gamma_source(&mut app, "annex");

        let claude = home.join(".claude/skills");
        let codex = home.join(".agents/skills");
        fs::create_dir_all(&claude).expect("create Claude Code root");
        fs::create_dir_all(&codex).expect("create Codex root");
        symlink(library.join("skills/gamma"), claude.join("gamma"));
        symlink(annex.join("skills/gamma"), codex.join("gamma"));

        scan_installations(&mut app);
        app
    }

    /// A committed checkout carrying `gamma`, registered as a source, whose
    /// path the caller installs from.
    #[cfg(unix)]
    fn registered_gamma_source(&self, app: &mut SkilledApp, name: &str) -> PathBuf {
        let repository = self.directory.path().join("home").join(name);
        write_skill_fixture(&repository.join("skills/gamma"), "gamma");
        create_repository(&repository);
        let preview = app.preview_source(&repository).expect("preview source");
        app.confirm_source(preview).expect("register source");
        repository
    }

    /// One skill, installed from a registered source, whose name and whose
    /// source's name both outrun the capped identity columns.
    #[cfg(unix)]
    fn long_name_inventory(&self) -> SkilledApp {
        let home = self.directory.path().join("home");
        let repository = home.join(LONG_SOURCE_DIRECTORY);
        let variant = repository.join("skills").join(LONG_SKILL_NAME);
        write_skill_fixture(&variant, LONG_SKILL_NAME);
        create_repository(&repository);

        let mut app = self.completed_setup();
        let preview = app.preview_source(&repository).expect("preview source");
        app.confirm_source(preview).expect("register source");

        let claude = home.join(".claude/skills");
        fs::create_dir_all(&claude).expect("create Claude Code root");
        symlink(variant, claude.join(LONG_SKILL_NAME));

        scan_installations(&mut app);
        app
    }

    /// The registration and installations the two provenance fixtures share,
    /// stopping short of the scan so the checkout can still be moved.
    #[cfg(unix)]
    fn gamma_installed_two_ways(&self) -> SkilledApp {
        let home = self.directory.path().join("home");
        let repository = home.join("library");
        write_skill_fixture(&repository.join("skills/gamma"), "gamma");
        create_repository(&repository);

        let mut app = self.completed_setup();
        let preview = app.preview_source(&repository).expect("preview source");
        app.confirm_source(preview).expect("register source");

        let claude = home.join(".claude/skills");
        let codex = home.join(".agents/skills");
        fs::create_dir_all(&claude).expect("create Claude Code root");
        fs::create_dir_all(&codex).expect("create Codex root");
        symlink(repository.join("skills/gamma"), claude.join("gamma"));
        write_skill_fixture(&codex.join("gamma"), "gamma");
        app
    }
}

#[cfg(unix)]
fn scan_installations(app: &mut SkilledApp) {
    app.update(Action::OpenSources);
    let update = app.update(Action::OpenInventory);
    app.perform_effects(update.effects())
        .expect("installation scan");
}

/// Longer than the thirty-five cells the capped Skill column can show, and
/// still a valid skill name: lowercase letters with single hyphen separators.
const LONG_SKILL_NAME: &str = "an-installed-skill-with-a-deliberately-long-name";
/// Longer than the twenty-three cells the capped Source column can show. A
/// source's label is its checkout's directory name.
const LONG_SOURCE_DIRECTORY: &str = "a-deliberately-long-source-checkout-name";

/// Commit a fixture checkout so it can be registered as a source.
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
    drawn(app, width, height).0
}

/// What a frame reported about itself, which is how the geometry the reducer
/// cannot see reaches the application state.
fn feedback(app: &SkilledApp, width: u16, height: u16) -> RenderFeedback {
    drawn(app, width, height).1
}

/// The scroll extent of a frame that drew the detail region.
fn measured_extent(app: &SkilledApp, width: u16, height: u16) -> usize {
    feedback(app, width, height)
        .detail_max_scroll()
        .expect("the frame drew the detail region")
}

/// Draw, take the frame's report back to the application, and move the window
/// — the loop in `runner::run`, so a test scrolls the way a user does.
fn scroll_detail(app: &mut SkilledApp, width: u16, height: u16, steps: usize) {
    for _ in 0..steps {
        if let Some(extent) = feedback(app, width, height).detail_max_scroll() {
            app.note_detail_max_scroll(extent);
        }
        app.update(Action::ScrollDetail(1));
    }
}

fn drawn(app: &SkilledApp, width: u16, height: u16) -> (Buffer, RenderFeedback) {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("create test terminal");
    let mut feedback = RenderFeedback::default();
    terminal
        .draw(|frame| feedback = skilled::tui::render(frame, app))
        .expect("render frame");
    (terminal.backend().buffer().clone(), feedback)
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
    create_repository(repository);
}

/// A checkout holding a common catalog and an agent-specific one, so grouping
/// has two groups to keep apart and two classifications to state.
fn create_two_catalog_source_fixture(repository: &Path) {
    for (catalog, skill, description) in [
        ("skills", "portable", "Portable fixture"),
        (
            "experimental/claude-code/skills",
            "experimental",
            "Experimental fixture",
        ),
    ] {
        let directory = repository.join(catalog).join(skill);
        fs::create_dir_all(&directory).expect("create catalog fixture");
        fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: {skill}\ndescription: {description}\n---\n# Fixture\n"),
        )
        .expect("write catalog fixture");
    }
    create_repository(repository);
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

    /// Doctor's severity column is a badge like every other: a glyph and a
    /// word carry the meaning, and the tone only reinforces it. The list is
    /// issue-first, so the order is checked beside the styling.
    #[test]
    fn doctor_severities_pair_their_colour_with_a_glyph_and_a_word() {
        let harness = Harness::new();
        let mut app = harness.resolution_inventory();
        let update = app.update(Action::OpenDoctor);
        app.perform_effects(update.effects())
            .expect("installation scan");

        // Wide enough that the primary region shows every code whole: a cell
        // test that matched a truncated prefix would not be testing the cell.
        let screen = buffer(&app, 120, 24);

        for (code, glyph, word, colour) in [
            (
                "variant.duplicate_for_agent",
                "×",
                "critical",
                Color::Rgb(0xee, 0x6b, 0x73),
            ),
            (
                "variant.foreign_opencode_exposure",
                "!",
                "warning",
                Color::Rgb(0xe6, 0xbd, 0x6a),
            ),
            (
                "variant.incompatible_for_opencode",
                "!",
                "warning",
                Color::Rgb(0xe6, 0xbd, 0x6a),
            ),
        ] {
            let row = row_containing(&screen, code);
            let line = row_text(&screen, row);
            assert!(
                line.contains(&format!("{glyph} {word}")),
                "{glyph} {word} missing from {line:?}"
            );
            assert_eq!(
                style_in_row(&screen, row, word).fg,
                Some(colour),
                "wrong tone for {code}"
            );
        }

        // Critical before warning: the list is read down.
        assert!(
            row_containing(&screen, "variant.duplicate_for_agent")
                < row_containing(&screen, "variant.foreign_opencode_exposure"),
            "{}",
            text(&screen)
        );
    }

    /// The two verdicts the effective resolution adds are cells like any
    /// other: a glyph and a word carry the meaning, and the tone only
    /// reinforces it.
    #[test]
    fn an_effective_resolution_verdict_pairs_its_colour_with_a_glyph_and_a_word() {
        let harness = Harness::new();
        let app = harness.resolution_inventory();

        let screen = buffer(&app, 80, 24);

        for (name, glyph, word, colour) in [
            ("review", "×", "conflict", Color::Rgb(0xee, 0x6b, 0x73)),
            ("exposed", "!", "foreign", Color::Rgb(0xe6, 0xbd, 0x6a)),
            (
                "excluded",
                "!",
                "incompatible",
                Color::Rgb(0xe6, 0xbd, 0x6a),
            ),
        ] {
            let row = row_containing(&screen, name);
            let line = row_text(&screen, row);
            assert!(
                line.contains(&format!("{glyph} {word}")),
                "{glyph} {word} missing from {line:?}"
            );
            assert_eq!(
                style_in_row(&screen, row, word).fg,
                Some(colour),
                "wrong tone for {name}"
            );
        }
    }

    /// The Source column sets back the labels that place content with no
    /// registered source — `not registered` and `unverified` — while the ones
    /// that place it with at least one, a source name or `mixed`, keep the
    /// body text.
    #[test]
    fn the_source_column_sets_back_labels_that_place_content_with_no_source() {
        const MUTED: Color = Color::Rgb(0x84, 0x91, 0xa1);
        const TEXT: Color = Color::Rgb(0xd7, 0xde, 0xe7);

        let installed = buffer(&Harness::new().installed_inventory(), 80, 24);
        let copied = row_containing(&installed, "copied");
        assert_eq!(
            style_in_row(&installed, copied, "not registered").fg,
            Some(MUTED),
            "an unregistered row should not read as a source name"
        );
        let alpha = row_containing(&installed, "alpha");
        assert_eq!(
            style_in_row(&installed, alpha, "library").fg,
            Some(TEXT),
            "a registered source label keeps the body text"
        );

        // "mixed" names no single source, but it still reports that one of the
        // installations came from a registered one.
        let mixed = buffer(&Harness::new().mixed_provenance_inventory(), 80, 24);
        let gamma = row_containing(&mixed, "gamma");
        assert_eq!(
            style_in_row(&mixed, gamma, "mixed").fg,
            Some(TEXT),
            "a mixed row places part of itself with a registered source"
        );

        // "multiple sources" names none of them either, but every one of its
        // installations came from a registered source. Sixteen cells of label
        // need a wider terminal than the rest of these: the Source column
        // ellipsizes it at the minimum size, and the row is found by its
        // marker because the detail region names the selected skill too.
        let divergent = buffer(&Harness::new().divergent_provenance_inventory(), 180, 40);
        let gamma = row_starting_with(&divergent, "▌ gamma");
        assert_eq!(
            style_in_row(&divergent, gamma, "multiple sources").fg,
            Some(TEXT),
            "a divergent row places all of itself with registered sources"
        );

        let unverified = buffer(&Harness::new().unverified_provenance_inventory(), 80, 24);
        let gamma = row_containing(&unverified, "gamma");
        assert_eq!(
            style_in_row(&unverified, gamma, "unverified").fg,
            Some(MUTED),
            "a row that places nothing should not read as a source name"
        );
    }

    /// Both halves of what the caps trade: the table ellipsizes what outruns
    /// a capped column, and the detail region is where the whole name and the
    /// whole source label still are.
    #[test]
    fn what_outruns_the_capped_columns_is_ellipsized_and_kept_in_the_detail() {
        let harness = Harness::new();
        let mut app = harness.long_name_inventory();

        let table = buffer(&app, 180, 40);
        let row = row_starting_with(&table, "▌ an-installed-skill");
        let line = row_text(&table, row);

        // `padded` bounds a cell to one less than its column, so thirty-five
        // cells of skill and twenty-three of source survive, the last three of
        // each spent on the ellipsis.
        assert!(
            line.contains(&format!("{}...", &LONG_SKILL_NAME[..32])),
            "{line:?}"
        );
        assert!(
            line.contains(&format!("{}...", &LONG_SOURCE_DIRECTORY[..20])),
            "{line:?}"
        );
        assert!(!line.contains(LONG_SKILL_NAME), "{line:?}");
        assert!(!line.contains(LONG_SOURCE_DIRECTORY), "{line:?}");

        // Drilled into, where the fields are not competing with five other
        // columns for width, both are given whole. The name is probed on the
        // title row under the SKILL kicker, not the whole screen: the pane
        // header names the skill too, and a bare `contains` would pass with
        // the title line deleted.
        app.update(Action::AdvanceInventoryPane);
        let drilled = buffer(&app, 80, 24);
        let kicker = row_containing(&drilled, "SKILL");
        assert_eq!(row_text(&drilled, kicker + 2).trim(), LONG_SKILL_NAME);
        let detail = text(&drilled);
        assert!(
            detail.contains(&format!("Source: {LONG_SOURCE_DIRECTORY}")),
            "{detail}"
        );
    }

    /// The aside takes its full share only where the table has nothing left
    /// to gain: every table column is the same on both sides of the
    /// threshold, so widening the terminal never ellipsizes a name that fit
    /// just before.
    #[test]
    fn crossing_the_wide_detail_threshold_never_shrinks_the_table() {
        let harness = Harness::new();
        let app = harness.long_name_inventory();

        let table_row = |width: u16| {
            let screen = buffer(&app, width, 40);
            let row = row_starting_with(&screen, "▌ an-installed-skill");
            let line = row_text(&screen, row);
            let separator = line.find('│').expect("detail separator");
            line[..separator].trim_end().to_owned()
        };

        let narrow_side = table_row(150);
        let wide_side = table_row(151);
        assert_eq!(
            narrow_side, wide_side,
            "the table gave up columns to the aside at the crossing"
        );
        // Both identity caps bind there, so the capped prefix is the same one
        // the very-wide screen shows.
        assert!(
            narrow_side.contains(&format!("{}...", &LONG_SKILL_NAME[..32])),
            "{narrow_side:?}"
        );
    }

    /// Capping the columns leaves slack to the right of Health, and the
    /// selection band still crosses it: a band that stopped where the content
    /// did would read as a row ending mid-region.
    #[test]
    fn the_selected_row_band_crosses_the_slack_left_by_the_capped_columns() {
        const SURFACE_3: Color = Color::Rgb(0x17, 0x21, 0x2c);

        let harness = Harness::new();
        let app = harness.installed_inventory();

        // By its marker, because the detail region names the selected skill
        // too and its header sits above the table.
        let screen = buffer(&app, 180, 40);
        let alpha = row_starting_with(&screen, "▌ alpha");
        let line = row_text(&screen, alpha);

        // The table region ends at the detail separator; its content ends at
        // the health badge, and everything between the two is slack.
        let separator = line.find('│').expect("detail separator");
        let separator = line[..separator].chars().count() as u16;
        let content = line.find("healthy").expect("health badge") + "healthy".len();
        let content = line[..content].chars().count() as u16;
        assert!(
            separator > content + 10,
            "expected slack beside the capped columns: {line:?}"
        );

        for column in [content + 1, separator - 1] {
            assert_eq!(
                screen[(column, alpha)].style().bg,
                Some(SURFACE_3),
                "the band should reach column {column} of {line:?}"
            );
        }
        // It stops at the table region, though: the detail region beside it is
        // not part of the row.
        assert_ne!(screen[(separator, alpha)].style().bg, Some(SURFACE_3));
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
        // not every listed entry. The '·' lead-in keeps the count from
        // reading as a route key beside it, so a copy change to either
        // number has to update this probe in lockstep.
        assert!(
            navigation.contains(&format!("▌Inventory ·{skills} ")),
            "{navigation}"
        );
        assert!(
            navigation.contains(&format!(" 2 Sources ·{sources} ")),
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
        assert!(navigation.contains("▌Inventory ·0 "), "{navigation}");
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

    /// The detail region leads with the skill's own name, set as a title, and
    /// the health badge beneath it: the badge words say what they mean, so
    /// neither line needs a field label repeating the column headings.
    #[test]
    fn the_detail_region_leads_with_the_skill_name_and_its_health() {
        const TEXT_STRONG: Color = Color::Rgb(0xf2, 0xf6, 0xfa);
        const GREEN: Color = Color::Rgb(0x8b, 0xd4, 0x9c);

        let harness = Harness::new();
        let app = harness.installed_inventory();

        let screen = buffer(&app, 120, 40);
        let rendered = text(&screen);
        assert!(!rendered.contains("Name: alpha"), "{rendered}");
        assert!(!rendered.contains("Health: "), "{rendered}");

        // The title sits between its kicker and the badge.
        let kicker = row_containing(&screen, "│ SKILL");
        let title = row_containing(&screen, "│ alpha");
        let badge = row_containing(&screen, "│ ✓ healthy");
        assert_eq!(title, kicker + 2, "{rendered}");
        assert_eq!(badge, title + 1, "{rendered}");

        // By position after the separator: the table beside the detail region
        // names the same skill on the same row.
        let title_style = style_following(&screen, title, "│ ");
        assert_eq!(title_style.fg, Some(TEXT_STRONG));
        assert!(title_style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(style_following(&screen, badge, "│ ").fg, Some(GREEN));
    }

    /// The detail region sits on its own surface, so the split reads as two
    /// regions rather than one table with an annotation beside it. The
    /// prototype keeps that surface in its narrow layout too, so the compact
    /// drill-in carries it as well.
    #[test]
    fn the_detail_region_sits_on_its_own_surface() {
        const DETAIL_SURFACE: Color = Color::Rgb(0x0c, 0x11, 0x17);

        let harness = Harness::new();
        let mut app = harness.installed_inventory();

        let screen = buffer(&app, 120, 40);
        let row = row_containing(&screen, "│ SKILL");
        let line = row_text(&screen, row);
        let separator = line.find('│').expect("detail separator");
        let separator = line[..separator].chars().count() as u16;
        assert_eq!(
            screen[(screen.area.x + separator + 2, row)].style().bg,
            Some(DETAIL_SURFACE)
        );
        // The surface is painted before the margin, so it reaches the last
        // column of the region — an inset paint would leave a stripe of the
        // application surface down the edge and this cell would catch it.
        assert_eq!(
            screen[(screen.area.x + screen.area.width - 1, row)]
                .style()
                .bg,
            Some(DETAIL_SURFACE)
        );
        // The table keeps the application surface: the separator is a boundary
        // between two backgrounds, not a line drawn on one.
        assert_ne!(
            screen[(screen.area.x + 2, row)].style().bg,
            Some(DETAIL_SURFACE)
        );

        app.update(Action::AdvanceInventoryPane);
        let drilled = buffer(&app, 80, 24);
        let row = row_containing(&drilled, "SKILL");
        assert_eq!(
            drilled[(drilled.area.x + 2, row)].style().bg,
            Some(DETAIL_SURFACE)
        );
    }

    /// Each agent's section says how that agent's own installation stands, the
    /// terminal equivalent of the prototype's tone-coloured section borders.
    /// The heading naming the agent stays muted; only the badge carries tone.
    #[test]
    fn each_agent_section_heading_carries_that_agents_own_health() {
        const MUTED: Color = Color::Rgb(0x84, 0x91, 0xa1);
        const VIOLET: Color = Color::Rgb(0xc7, 0x9b, 0xf2);
        const RED: Color = Color::Rgb(0xee, 0x6b, 0x73);

        let harness = Harness::new();
        let mut app = harness.installed_inventory();

        // `broken`: one Claude Code installation whose link dangles.
        app.update(Action::MoveInventorySelection(1));
        let screen = buffer(&app, 120, 40);
        let heading = row_containing(&screen, "│ CLAUDE CODE");
        let line = row_text(&screen, heading);
        assert!(line.contains("CLAUDE CODE  × broken"), "{line:?}");
        // Probed past the separator, not by bare text: the table beside the
        // region sets the same words in the same colours, and a probe that
        // searched the whole row could drift onto them and test nothing.
        assert_eq!(style_following(&screen, heading, "│ ").fg, Some(MUTED));
        assert_eq!(
            style_following(&screen, heading, "CLAUDE CODE  ").fg,
            Some(RED)
        );

        // `copied`: a Codex installation that resolved to no registered source.
        app.update(Action::MoveInventorySelection(1));
        let screen = buffer(&app, 120, 40);
        let heading = row_containing(&screen, "│ CODEX");
        let line = row_text(&screen, heading);
        assert!(line.contains("CODEX  U unmanaged"), "{line:?}");
        assert_eq!(style_following(&screen, heading, "│ ").fg, Some(MUTED));
        assert_eq!(
            style_following(&screen, heading, "CODEX  ").fg,
            Some(VIOLET)
        );
    }

    /// The detail region's kickers name their sections, so they are set in the
    /// readable muted grey rather than the cyan reserved for focus accents.
    #[test]
    fn detail_section_headings_are_muted_rather_than_a_focus_accent() {
        const MUTED: Color = Color::Rgb(0x84, 0x91, 0xa1);

        let harness = Harness::new();
        let app = harness.installed_inventory();

        let screen = buffer(&app, 120, 40);
        // Probed past the separator: the table's own SKILL heading is muted
        // too, and a whole-row search could land on it instead.
        let kicker = row_containing(&screen, "│ SKILL");
        assert_eq!(style_following(&screen, kicker, "│ ").fg, Some(MUTED));

        let source = row_containing(&screen, "│ SOURCE");
        assert_eq!(style_following(&screen, source, "│ ").fg, Some(MUTED));
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
        // The region has the keyboard and rows below the window, so the notice
        // names the keys that reach them rather than advising a bigger
        // terminal: the binding is live, so the advice is one we can keep.
        assert!(line.contains("below — j/k to scroll"), "{line}");
        assert_eq!(
            style_in_row(&screen, notice, "!").fg,
            Some(Color::Rgb(0xe6, 0xbd, 0x6a))
        );
    }

    /// `update` never learns the terminal's size, so the frame measures how far
    /// its detail region can usefully scroll and reports it back. A region tall
    /// enough for everything reports nothing to scroll to, which is what keeps
    /// the offset — and the key hint that depends on it — from claiming a
    /// movement the screen cannot make.
    #[test]
    fn the_detail_region_reports_the_extent_the_frame_could_scroll() {
        let harness = Harness::new();
        let mut app = harness.everywhere_installed_inventory();
        app.update(Action::AdvanceInventoryPane);

        let cramped = measured_extent(&app, 80, 24);
        assert!(
            cramped > 0,
            "three agent sections outgrow the minimum terminal\n{}",
            text(&buffer(&app, 80, 24))
        );
        assert_eq!(measured_extent(&app, 80, 60), 0);

        // A compact terminal showing the table has not measured the region
        // behind it, which is not the same as measuring nothing to scroll.
        app.update(Action::Back);
        assert_eq!(
            feedback(&app, 80, 24).detail_max_scroll(),
            None,
            "the region is not on screen"
        );
        app.update(Action::AdvanceInventoryPane);

        // Scrolling as far as the frame reported reaches the last agent's
        // section, which is what makes the extent an extent rather than a
        // number: unreachable rows are the whole complaint behind the region.
        // The OpenCode agent's own section, not the row's resolution section:
        // the latter states what OpenCode would load and stands above the fold.
        let cut = text(&buffer(&app, 80, 24));
        assert!(
            !cut.contains("Path: ~/.config/opencode/skills/alpha"),
            "{cut}"
        );
        scroll_detail(&mut app, 80, 24, cramped);
        let scrolled = text(&buffer(&app, 80, 24));
        assert!(
            scrolled.contains("Path: ~/.config/opencode/skills/alpha"),
            "{scrolled}"
        );
    }

    /// The counts at the two ends of the window are measurements, so they are
    /// checked against the screen rather than against themselves: at every
    /// height the region can be cut at and every offset it can be scrolled to,
    /// what it says is above, plus what it shows, plus what it says is below,
    /// is the whole of the content. A stated count that drifts from the rows on
    /// screen is worse than no notice, because it is read as a fact.
    #[test]
    fn the_scrolled_detail_region_accounts_for_every_row_it_does_not_show() {
        fn region_rows(app: &SkilledApp, width: u16, height: u16) -> Vec<String> {
            let screen = text(&buffer(app, width, height));
            let screen: Vec<&str> = screen.lines().collect();
            let mut rows: Vec<String> = if screen.iter().any(|row| row.contains('│')) {
                // Beside the table, the region is everything past the rule.
                screen
                    .iter()
                    .filter_map(|row| row.rsplit_once('│'))
                    .map(|(_, region)| region.trim().to_owned())
                    .collect()
            } else {
                // Drilled into on a compact terminal, it is the whole
                // workspace: below the title bar and navigation, above the
                // key hints.
                screen[2..screen.len() - 1]
                    .iter()
                    .map(|row| row.trim().to_owned())
                    .collect()
            };
            // The pane's own heading and rule are the scaffold around the
            // window, not rows of the content it is windowing.
            rows.drain(..2);
            // Trailing blanks are room the region did not need, not content
            // it showed. Blanks between sections are content and stay counted,
            // because a hidden one costs the reader a row like any other.
            while rows.last().is_some_and(String::is_empty) {
                rows.pop();
            }
            rows
        }
        fn stated(row: &str, phrase: &str) -> Option<usize> {
            row.strip_prefix("! ")?
                .split_once(phrase)
                .and_then(|(count, _)| count.trim().parse().ok())
        }

        let harness = Harness::new();
        let mut app = harness.everywhere_installed_inventory();
        app.update(Action::AdvanceInventoryPane);
        let mut cut_heights = 0;

        // Three widths: the two sides of the wide breakpoint, where the region
        // is beside the table and wraps differently, and the minimum supported
        // terminal, where it is drilled into and fills the workspace.
        for width in [120, 100, 80] {
            let whole = region_rows(&app, width, 80);
            for height in 24..40 {
                let extent = measured_extent(&app, width, height);
                app.note_detail_max_scroll(extent);
                for _ in 0..=extent {
                    app.update(Action::ScrollDetail(-1));
                }
                let mut previously_above = 0;
                for offset in 0..=extent {
                    if offset > 0 {
                        app.update(Action::ScrollDetail(1));
                    }
                    assert_eq!(app.detail_scroll(), offset);

                    let rows = region_rows(&app, width, height);
                    let above = rows.first().and_then(|row| stated(row, " line"));
                    let below = rows.last().and_then(|row| stated(row, " more line"));
                    // No line of this region's content is blank, so a blank
                    // row above the notice is room a whole line could not
                    // fill: unused space, like the tail of an uncut region,
                    // and not a row the window showed.
                    let mut shown = &rows
                        [usize::from(above.is_some())..rows.len() - usize::from(below.is_some())];
                    while shown.last().is_some_and(String::is_empty) {
                        shown = &shown[..shown.len() - 1];
                    }
                    if below.is_some() {
                        cut_heights += 1;
                    }
                    assert_eq!(
                        above.unwrap_or(0) + shown.len() + below.unwrap_or(0),
                        whole.len(),
                        "at {width} columns, height {height} and offset {offset} the \
                         region showed {} rows and claimed {above:?} above and \
                         {below:?} below",
                        shown.len()
                    );
                    // The rows themselves, not just their number: a window
                    // drawn a row away from the count it states would add up
                    // correctly and still be showing the wrong place.
                    let above = above.unwrap_or(0);
                    assert_eq!(
                        shown,
                        &whole[above..above + shown.len()],
                        "at {width} columns, height {height} and offset {offset} the \
                         window opened somewhere other than where it says"
                    );
                    // Every keystroke is worth pressing: one step of the offset
                    // leaves one more line behind, so the window always moves.
                    assert!(
                        offset == 0 || above > previously_above,
                        "at {width} columns and height {height} the window did not \
                         move for offset {offset}"
                    );
                    previously_above = above;
                    // A field wrapped onto a second row is withheld rather
                    // than cut after its label: a path stated as empty is a
                    // false observation, and this region's only job is to
                    // state what was observed.
                    assert!(
                        !shown.last().is_some_and(|row| row.ends_with(':')),
                        "at {width} columns, height {height} and offset {offset} the \
                         window ended on a label with no value: {:?}",
                        shown.last()
                    );
                }
                // Scrolled to the extent, there is nothing left below: an
                // extent that never reaches the end is not an extent.
                if extent > 0 {
                    let rows = region_rows(&app, width, height);
                    assert_eq!(
                        rows.last().and_then(|row| stated(row, " more line")),
                        None,
                        "at {width} columns and height {height} the last offset still \
                         hid rows"
                    );
                }
            }
        }
        assert!(
            cut_heights > 0,
            "the sweep should reach heights where the region is cut"
        );
    }

    /// Colour is not a signal a terminal can be relied on to carry, so both
    /// ends of the window are words as well as a tone — and the tone is the
    /// warning role, the same one the uncut region's notice already wears.
    #[test]
    fn both_ends_of_the_scrolled_detail_region_are_warning_badges() {
        const WARNING: Color = Color::Rgb(0xe6, 0xbd, 0x6a);

        let harness = Harness::new();
        let mut app = harness.everywhere_installed_inventory();
        app.update(Action::AdvanceInventoryPane);
        scroll_detail(&mut app, 80, 24, 1);

        let screen = buffer(&app, 80, 24);
        let above = row_containing(&screen, "line above");
        assert!(
            row_text(&screen, above).contains("! 1 line above"),
            "{}",
            row_text(&screen, above)
        );
        assert_eq!(style_in_row(&screen, above, "!").fg, Some(WARNING));

        let below = row_containing(&screen, "more line");
        assert!(
            row_text(&screen, below).contains("below"),
            "{}",
            row_text(&screen, below)
        );
        assert_eq!(style_in_row(&screen, below, "!").fg, Some(WARNING));
    }

    /// The reducer can only clamp the offset against the frame before it, so a
    /// terminal that grew since then hands the renderer an offset past the end
    /// of what it now has to show. The window comes back to the end rather
    /// than opening on the blank beyond it, which a reader takes for an
    /// absence of content.
    #[test]
    fn a_frame_handed_a_stale_offset_still_opens_on_content() {
        let harness = Harness::new();
        let mut app = harness.everywhere_installed_inventory();
        app.update(Action::AdvanceInventoryPane);
        let cramped = measured_extent(&app, 80, 24);
        scroll_detail(&mut app, 80, 24, cramped);

        // A taller region reaches the same last row from a smaller offset, so
        // the one the application is holding is now past the end.
        let stale = app.detail_scroll();
        let extent = measured_extent(&app, 80, 30);
        assert!(extent > 0, "the taller region should still be cut");
        assert!(stale > extent, "{stale} should outrun {extent}");

        // The frame draws what it would have drawn had the offset been pulled
        // back first, and the next one — the runner notes before every key —
        // pulls it back for good.
        let stale_frame = text(&buffer(&app, 80, 30));
        app.note_detail_max_scroll(extent);
        assert_eq!(app.detail_scroll(), extent);
        assert_eq!(stale_frame, text(&buffer(&app, 80, 30)));

        assert!(stale_frame.contains("OPENCODE"), "{stale_frame}");
        assert!(stale_frame.contains("lines above"), "{stale_frame}");
        assert!(!stale_frame.contains("more lines below"), "{stale_frame}");
    }

    /// Moving focus out of the region and back changes nothing behind it, so
    /// the window is where it was left. A compact terminal takes the region
    /// off screen entirely while the table has focus, and a frame that
    /// measured nothing must not be read as one that measured nothing to
    /// scroll — the offset would be lost on the way past.
    #[test]
    fn the_window_survives_leaving_the_region_in_either_viewport() {
        for (width, height) in [(80, 24), (120, 24)] {
            let harness = Harness::new();
            let mut app = harness.everywhere_installed_inventory();
            app.update(Action::AdvanceInventoryPane);
            scroll_detail(&mut app, width, height, 2);
            assert_eq!(app.detail_scroll(), 2, "at {width}x{height}");

            // Drawn between each step, the way the runner does it.
            for _ in 0..2 {
                if let Some(extent) = feedback(&app, width, height).detail_max_scroll() {
                    app.note_detail_max_scroll(extent);
                }
                app.update(Action::MoveInventoryPane(1));
            }

            assert_eq!(app.inventory_pane(), InventoryPane::Details);
            assert_eq!(app.detail_scroll(), 2, "at {width}x{height}");
        }
    }

    /// A key hint and a help entry are contracts: they appear exactly where the
    /// binding does something. The detail region's window only moves where the
    /// region has the keyboard and this frame found rows it does not reach.
    #[test]
    fn the_scroll_affordance_appears_only_where_the_window_can_move() {
        let harness = Harness::new();
        let mut app = harness.everywhere_installed_inventory();

        // Beside the table, the region is cut but the keys belong to the table.
        let beside = text(&buffer(&app, 100, 24));
        assert!(!beside.contains("j/k Scroll"), "{beside}");

        app.update(Action::AdvanceInventoryPane);
        let cut = text(&buffer(&app, 80, 24));
        assert!(cut.contains("j/k Scroll"), "{cut}");
        assert!(!cut.contains("j/k Move"), "{cut}");

        // A region tall enough for everything has nowhere to scroll to.
        let whole = text(&buffer(&app, 80, 60));
        assert!(!whole.contains("j/k Scroll"), "{whole}");

        app.update(Action::OpenHelp);
        let help = text(&buffer(&app, 80, 24));
        assert!(help.contains("Scroll details"), "{help}");
        let tall = text(&buffer(&app, 80, 60));
        assert!(!tall.contains("Scroll details"), "{tall}");
    }

    /// The notice below the window names the way to the rows beneath it, so it
    /// answers for the same contract a key hint does. A dialog holds the
    /// keyboard while it is open and the filter bar takes every printable key,
    /// so under either the notice names no keystroke at all — both screens
    /// already say navigation is locked, and a notice pointing at a movement
    /// key would contradict them about the very rows it is reporting on.
    #[test]
    fn the_notice_names_no_keys_a_dialog_or_the_filter_bar_has_taken() {
        fn notice(app: &SkilledApp) -> String {
            let screen = buffer(app, 120, 24);
            row_text(&screen, row_containing(&screen, "more line"))
        }
        let harness = Harness::new();
        let mut app = harness.everywhere_installed_inventory();

        assert!(notice(&app).contains("Tab, then j/k"), "{}", notice(&app));

        app.update(Action::OpenHelp);
        let with_help = notice(&app);
        assert!(with_help.contains("more line"), "{with_help}");
        assert!(!with_help.contains("j/k"), "{with_help}");
        assert!(!with_help.contains("Tab"), "{with_help}");

        app.update(Action::CloseHelp);
        app.update(Action::AdvanceInventoryPane);
        assert!(notice(&app).contains("j/k to scroll"), "{}", notice(&app));

        // Drilled in, where the movement keys really are the region's, a
        // dialog still answers for them first.
        app.update(Action::OpenHelp);
        let drilled_in = notice(&app);
        assert!(!drilled_in.contains("j/k"), "{drilled_in}");
        app.update(Action::CloseHelp);

        app.update(Action::MoveInventoryPane(1));
        app.update(Action::BeginInventoryFilter);
        assert!(app.inventory_filter_active());
        let filtering = notice(&app);
        assert!(filtering.contains("more line"), "{filtering}");
        assert!(!filtering.contains("j/k"), "{filtering}");
        assert!(!filtering.contains("Tab"), "{filtering}");
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
        assert!(!table.contains("Description: alpha fixture"), "{table}");

        app.update(Action::AdvanceInventoryPane);
        let detail = text(&buffer(&app, 80, 24));
        assert!(detail.contains("▌ Details  alpha"), "{detail}");
        assert!(detail.contains("Description: alpha fixture"), "{detail}");
        assert!(!detail.contains("Global inventory"), "{detail}");
        // Neither selection nor filtering acts in the detail region — the
        // query box is drawn above the table, which is not on screen — so the
        // bar must not advertise either. The movement keys are bound here all
        // the same, to the window of a region taller than this terminal, and
        // the bar says which of the two they do. Naming it costs the row its
        // last non-essential hints, which is what the bar's own budget is for.
        assert_eq!(
            row_text(&buffer(&app, 80, 24), 23),
            " Tab/Shift-Tab Region   j/k Scroll   2 Sources   4 Doctor   q Quit   Esc Back …"
        );

        app.update(Action::Back);
        let back = text(&buffer(&app, 80, 24));
        assert!(back.contains("▌ Global inventory"), "{back}");
    }
}
