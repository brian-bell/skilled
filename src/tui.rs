use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap},
};

use crate::{SetupStep, SkilledApp, View};

pub const MINIMUM_WIDTH: u16 = 80;
pub const MINIMUM_HEIGHT: u16 = 24;

pub fn render(frame: &mut Frame<'_>, app: &SkilledApp) {
    let area = frame.area();
    if area.width < MINIMUM_WIDTH || area.height < MINIMUM_HEIGHT {
        render_size_notice(frame, area);
        return;
    }

    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);

    render_header(frame, header, app.view());
    match app.view() {
        View::Setup(step) => render_setup(frame, body, app, step),
        View::Inventory => render_inventory(frame, body),
        View::Settings => {
            render_inventory(frame, body);
            render_settings(frame, area);
        }
    }
    render_footer(frame, footer, app.view());
}

fn render_header(frame: &mut Frame<'_>, area: Rect, view: View) {
    let section = match view {
        View::Setup(step) => format!("Setup · {}", step.title()),
        View::Inventory => "Inventory".to_owned(),
        View::Settings => "Inventory · Settings".to_owned(),
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " Skilled ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(section, Style::default().add_modifier(Modifier::BOLD)),
        ])),
        area,
    );
}

fn render_setup(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp, step: SetupStep) {
    let block = Block::default()
        .title(format!(" Step {} of 7 ", step.number()))
        .borders(Borders::ALL)
        .padding(Padding::new(2, 2, 1, 1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = setup_lines(app, step);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn setup_lines(app: &SkilledApp, step: SetupStep) -> Vec<Line<'static>> {
    let title = Line::styled(
        step.title(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    let mut lines = vec![title, Line::default()];
    match step {
        SetupStep::Welcome => {
            lines.extend([
                Line::from("Skilled manages global skills for Claude Code, Codex, and OpenCode."),
                Line::from("It inventories local state and previews every filesystem mutation."),
                Line::default(),
                Line::from("No coding agent is launched during setup or diagnosis."),
                Line::from("Existing physical files, directories, and unknown links are never overwritten."),
            ]);
        }
        SetupStep::DetectAgents => {
            lines.push(Line::from("All supported agents are selected by default."));
            lines.push(Line::default());
            for (index, detection) in app.agents().iter().enumerate() {
                let executable = detection
                    .executable_path()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "executable not found".to_owned());
                let root = if detection.root_exists() {
                    "root found"
                } else {
                    "root not found"
                };
                lines.push(Line::from(format!(
                    "{} [{}] {:<12} {:<18} {}",
                    if index == app.focused_agent() {
                        ">"
                    } else {
                        " "
                    },
                    if detection.selected() { "x" } else { " " },
                    detection.kind().display_name(),
                    root,
                    executable
                )));
            }
        }
        SetupStep::ChooseScanRoots => lines.extend([
            Line::from("No development scan roots are selected."),
            Line::from("Source-root selection is introduced by the next implementation slice."),
        ]),
        SetupStep::DiscoverSources => lines.extend([
            Line::from("No local source repositories discovered."),
            Line::from("Skilled never scans the entire home directory by default."),
        ]),
        SetupStep::ConfirmCatalogs => lines.extend([
            Line::from("No catalog roots require confirmation."),
            Line::from("Catalog registration and installation remain separate actions."),
        ]),
        SetupStep::ScanInstallations => lines.extend([
            Line::from("No configured installation roots have been scanned yet."),
            Line::from("Filesystem inventory is introduced by a later implementation slice."),
        ]),
        SetupStep::Summary => lines.extend([
            Line::from("Setup is ready to finish."),
            Line::default(),
            Line::from(format!(
                "Configured agents: {}",
                app.agents().iter().filter(|agent| agent.selected()).count()
            )),
            Line::from("Sources: 0   Skills: 0   Installations: 0   Doctor findings: 0"),
            Line::default(),
            Line::from("Unresolved findings never force a repair."),
        ]),
    }
    lines
}

fn render_inventory(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .title(" Inventory ")
        .borders(Borders::ALL)
        .padding(Padding::new(2, 2, 1, 1));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                "Installed skills",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Line::default(),
            Line::from("No installed skills found."),
            Line::from("Doctor findings: 0"),
        ]),
        inner,
    );
}

fn render_settings(frame: &mut Frame<'_>, area: Rect) {
    let popup = centered_rect(52, 9, area);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled("Settings", Style::default().add_modifier(Modifier::BOLD)),
            Line::default(),
            Line::from("> Rerun setup"),
            Line::default(),
            Line::from("Enter confirms · Esc closes"),
        ])
        .block(
            Block::default()
                .title(" Settings ")
                .borders(Borders::ALL)
                .padding(Padding::new(2, 2, 1, 1)),
        ),
        popup,
    );
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, view: View) {
    let hints = match view {
        View::Setup(SetupStep::DetectAgents) => {
            " j/k Move   Space Toggle   Enter Continue   Esc Back   q Quit "
        }
        View::Setup(_) => " Enter Continue   Esc Back   q Quit ",
        View::Inventory => " s Settings   ? Help   q Quit ",
        View::Settings => " Enter Rerun setup   Esc Close ",
    };
    frame.render_widget(
        Paragraph::new(hints)
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn render_size_notice(frame: &mut Frame<'_>, area: Rect) {
    let message = format!(
        "Skilled needs at least {MINIMUM_WIDTH}×{MINIMUM_HEIGHT}. Current size: {}×{}. Resize the terminal to continue.",
        area.width, area.height
    );
    frame.render_widget(
        Paragraph::new(message)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .title(" Terminal too small ")
                    .borders(Borders::ALL)
                    .padding(Padding::horizontal(1)),
            ),
        area,
    );
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}
