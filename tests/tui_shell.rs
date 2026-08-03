use ratatui::{
    Terminal,
    backend::TestBackend,
    buffer::Buffer,
    style::{Color, Modifier, Style},
};
use std::{fs, path::Path, process::Command};

use skilled::{Action, AppEnvironment, SkilledApp};

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
        row_text(&screen, 1).contains("1 Inventory"),
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
fn navigation_separates_active_reachable_and_unavailable_destinations() {
    let harness = Harness::new();
    let app = harness.completed_setup();

    let screen = buffer(&app, 80, 24);
    let navigation = row_text(&screen, 1);

    // Focus is carried by a marker and emphasis, not by colour alone.
    assert!(navigation.contains("▌1 Inventory"), "{navigation}");
    assert!(navigation.contains(" 2 Sources"), "{navigation}");
    assert!(!navigation.contains("▌2 Sources"), "{navigation}");

    let active = style_at(&screen, "1 Inventory");
    assert_eq!(active.fg, Some(Color::Rgb(0xf2, 0xf6, 0xfa)));
    assert!(active.add_modifier.contains(Modifier::BOLD));
    assert!(active.add_modifier.contains(Modifier::UNDERLINED));

    let reachable = style_at(&screen, "2 Sources");
    assert_eq!(reachable.fg, Some(Color::Rgb(0x84, 0x91, 0xa1)));
    assert!(!reachable.add_modifier.contains(Modifier::BOLD));

    // Views without an implementation are visibly unavailable rather than absent.
    assert!(navigation.contains("3 Updates (soon)"), "{navigation}");
    assert!(navigation.contains("4 Doctor (soon)"), "{navigation}");

    let unavailable = style_at(&screen, "3 Updates");
    assert_eq!(unavailable.fg, Some(Color::Rgb(0x53, 0x61, 0x71)));
    assert!(unavailable.add_modifier.contains(Modifier::DIM));
}

#[test]
fn navigation_does_not_offer_routes_that_setup_blocks() {
    let harness = Harness::new();
    let mut app = harness.first_run();
    app.update(Action::Continue);

    let navigation = row_text(&buffer(&app, 80, 24), 1);

    // Keys 1 and 2 do nothing during setup, so no tab may look reachable.
    assert!(!navigation.contains("1 Inventory"), "{navigation}");
    assert!(!navigation.contains("2 Sources"), "{navigation}");
    // The row still carries the persistent frame's sense of place.
    assert!(navigation.contains("Setup · Detect agents"), "{navigation}");
    assert!(
        navigation.contains("navigation unlocks after setup"),
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

    assert!(navigation.contains("▌2 Sources"), "{navigation}");
    assert!(!navigation.contains("▌1 Inventory"), "{navigation}");
}

#[test]
fn key_hints_advertise_only_commands_this_release_implements() {
    let harness = Harness::new();
    let app = harness.completed_setup();

    let hints = row_text(&buffer(&app, 80, 24), 23);

    assert!(hints.contains("2 Sources"), "{hints}");
    assert!(hints.contains("s Settings"), "{hints}");
    assert!(hints.contains("q Quit"), "{hints}");
    // Contextual help arrives with a later slice; nothing may promise it yet.
    assert!(!hints.contains("Help"), "{hints}");
    for absent in ["Install", "Uninstall", "Repair", "Update", "Filter"] {
        assert!(!hints.contains(absent), "{absent} in {hints}");
    }
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
    assert!(screen.contains("0 skills"), "{screen}");
    assert!(screen.contains("No installed skills found"), "{screen}");
    assert!(
        screen.contains("Skilled has not scanned any installation root yet."),
        "{screen}"
    );

    // Doctor, updates, and installation do not exist yet, so the empty state
    // may not report their results or offer their actions.
    for invented in [
        "Doctor findings",
        "findings: 0",
        "Install",
        "Uninstall",
        "Repair",
        "Update available",
        "healthy",
    ] {
        assert!(!screen.contains(invented), "{invented} in\n{screen}");
    }
}

#[test]
fn wide_terminals_gain_a_detail_region_and_compact_ones_do_not() {
    let harness = Harness::new();
    let app = harness.completed_setup();

    let wide = text(&buffer(&app, 120, 40));
    assert!(wide.contains("No selection"), "{wide}");
    assert!(wide.contains("Select a skill to see identity,"), "{wide}");
    assert!(
        wide.contains("provenance, and installation paths."),
        "{wide}"
    );
    // Both regions are present, so the primary empty state still reads.
    assert!(wide.contains("No installed skills found"), "{wide}");

    let compact = text(&buffer(&app, 80, 24));
    assert!(!compact.contains("No selection"), "{compact}");
    assert!(compact.contains("No installed skills found"), "{compact}");
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
    assert!(rendered.contains("┌"), "{rendered}");
    assert!(rendered.contains("┘"), "{rendered}");
    let border = row_containing(&screen, "Settings");
    assert_eq!(
        style_in_row(&screen, border, "┌").fg,
        Some(Color::Rgb(0x43, 0x52, 0x64))
    );

    // The header names the dialog and its scope; the body states the way out.
    assert!(rendered.contains("Settings"), "{rendered}");
    assert!(rendered.contains("global scope"), "{rendered}");
    assert!(rendered.contains("Esc closes"), "{rendered}");

    let title = style_at(&screen, "Settings");
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

    assert!(rendered.contains("Add source"), "{rendered}");
    assert!(rendered.contains("local Git checkout"), "{rendered}");
    let border = row_containing(&screen, "Add source");
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
