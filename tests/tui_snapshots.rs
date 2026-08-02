use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use skilled::{AppEnvironment, SkilledApp};

#[test]
fn first_run_welcome_at_minimum_supported_size() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let app = SkilledApp::open(AppEnvironment::new(
        "/tmp/skilled-test-home",
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
        "/tmp/skilled-test-home",
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");

    insta::assert_snapshot!(render(&app, 120, 40));
}

#[test]
fn undersized_terminal_shows_a_recoverable_notice() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let app = SkilledApp::open(AppEnvironment::new(
        "/tmp/skilled-test-home",
        temporary.path().join("data"),
        "",
    ))
    .expect("open application");

    insta::assert_snapshot!(render(&app, 60, 14));
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
