use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap},
};

use crate::{
    SetupStep, SkilledApp, SourcesPane, View,
    source::{CatalogClassification, SkillValidation},
};

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
        View::Sources => render_sources(frame, body, app),
        View::Settings => {
            render_inventory(frame, body);
            render_settings(frame, area);
        }
    }
    if app.source_path_input_active() {
        render_source_path_entry(frame, area, app);
    } else if app.pending_source().is_some() && app.view() == View::Sources {
        render_catalog_confirmation(frame, area, app);
    }
    render_footer(frame, footer, app);
}

fn render_header(frame: &mut Frame<'_>, area: Rect, view: View) {
    let section = match view {
        View::Setup(step) => format!("Setup · {}", step.title()),
        View::Inventory => "Inventory".to_owned(),
        View::Sources => "Sources".to_owned(),
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
        SetupStep::ConfirmCatalogs => {
            if app.pending_source().is_some() {
                lines.extend(catalog_confirmation_lines(app));
            } else {
                lines.extend([
                    Line::from("No catalog roots require confirmation."),
                    Line::from("Catalog registration and installation remain separate actions."),
                ]);
            }
        }
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
            Line::from(format!(
                "Sources: {}   Skills: {}   Installations: 0   Doctor findings: 0",
                app.sources().len(),
                app.sources()
                    .iter()
                    .flat_map(|source| source.catalogs())
                    .map(|catalog| catalog.candidates().len())
                    .sum::<usize>()
            )),
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

fn render_sources(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp) {
    let [repositories, _, details] = Layout::horizontal([
        Constraint::Percentage(34),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(area);
    let repository_block = Block::default()
        .title(" Repositories ")
        .borders(Borders::ALL)
        .border_style(if app.sources_pane() == SourcesPane::Repositories {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        })
        .padding(Padding::horizontal(1));
    let repository_inner = repository_block.inner(repositories);
    frame.render_widget(repository_block, repositories);
    let mut repository_lines = Vec::new();
    if app.sources().is_empty() {
        repository_lines.extend([
            Line::from("No registered sources."),
            Line::from("Press a to add one."),
        ]);
    } else {
        let capacity = usize::from(repository_inner.height.max(1));
        let start = visible_window_start(app.focused_source(), capacity);
        for (index, source) in app.sources().iter().enumerate().skip(start).take(capacity) {
            repository_lines.push(Line::from(format!(
                "{} {}  {}",
                if index == app.focused_source() {
                    ">"
                } else {
                    " "
                },
                source.label(),
                if source.dirty() { "dirty" } else { "clean" }
            )));
        }
    }
    frame.render_widget(Paragraph::new(repository_lines), repository_inner);

    let detail_block = Block::default()
        .title(" Available variants ")
        .borders(Borders::ALL)
        .border_style(if app.sources_pane() == SourcesPane::Variants {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        })
        .padding(Padding::horizontal(1));
    let detail_inner = detail_block.inner(details);
    frame.render_widget(detail_block, details);
    let Some(source) = app.selected_source() else {
        frame.render_widget(Paragraph::new("Select Add Source to begin."), detail_inner);
        return;
    };
    let mut lines = vec![
        Line::styled(
            source.git_top_level().display().to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::from(format!(
            "{} · {} · {} · scanned {}",
            source.branch().unwrap_or("detached"),
            &source.head()[..source.head().len().min(8)],
            if source.dirty() { "dirty" } else { "clean" },
            source.last_scan_at()
        )),
    ];
    if let Some(remote) = source.remote_url() {
        lines.push(Line::from(format!("Remote: {remote}")));
    }
    if let Some(error) = source.source_error() {
        lines.push(Line::styled(
            format!("× Source unavailable — {error}"),
            Style::default().fg(Color::Red),
        ));
    } else {
        for catalog in source.catalogs() {
            if let Some(error) = catalog.scan_error() {
                lines.push(Line::styled(
                    format!("× {} — {error}", catalog.relative_path().display()),
                    Style::default().fg(Color::Red),
                ));
            }
        }
    }
    lines.push(Line::default());
    let variants = source
        .catalogs()
        .iter()
        .flat_map(|catalog| {
            catalog
                .candidates()
                .iter()
                .map(move |candidate| (catalog, candidate))
        })
        .collect::<Vec<_>>();
    let reserved = lines.len().saturating_add(4);
    let capacity = usize::from(detail_inner.height)
        .saturating_sub(reserved)
        .max(1);
    let start = visible_window_start(app.focused_variant(), capacity);
    for (index, (catalog, candidate)) in variants.iter().enumerate().skip(start).take(capacity) {
        let status = if candidate.validation().is_valid() {
            "✓"
        } else {
            "×"
        };
        lines.push(Line::from(format!(
            "{} {status} {}  ({})",
            if index == app.focused_variant() {
                ">"
            } else {
                " "
            },
            candidate.directory_name(),
            catalog.relative_path().display()
        )));
    }
    if let Some((catalog, selected)) = variants.get(app.focused_variant()) {
        lines.push(Line::default());
        lines.push(Line::from(match catalog.classification() {
            CatalogClassification::Common => "Common catalog".to_owned(),
            CatalogClassification::AgentSpecific => "Agent-specific catalog".to_owned(),
        }));
        let compatibility = catalog.compatibility();
        lines.push(Line::from(format!(
            "Claude: {} · Codex: {} · OpenCode: {}",
            yes_no(compatibility.claude_code()),
            yes_no(compatibility.codex()),
            yes_no(compatibility.opencode())
        )));
        match selected.validation() {
            SkillValidation::Valid { description, .. } => {
                lines.push(Line::from(description.to_owned()));
            }
            SkillValidation::Invalid { message } => {
                lines.push(Line::styled(
                    message.to_owned(),
                    Style::default().fg(Color::Red),
                ));
            }
        }
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        detail_inner,
    );
}

fn visible_window_start(focused: usize, capacity: usize) -> usize {
    focused.saturating_add(1).saturating_sub(capacity)
}

fn render_source_path_entry(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp) {
    let popup = centered_rect(68, 9, area);
    frame.render_widget(Clear, popup);
    let mut lines = vec![
        Line::from("Enter a path inside a local Git checkout:"),
        Line::default(),
        Line::from(format!("> {}", app.source_path())),
    ];
    if let Some(error) = app.source_error() {
        lines.push(Line::default());
        lines.push(Line::styled(
            error.to_owned(),
            Style::default().fg(Color::Red),
        ));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .title(" Add source ")
                .borders(Borders::ALL)
                .padding(Padding::new(2, 2, 1, 1)),
        ),
        popup,
    );
}

fn render_catalog_confirmation(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp) {
    let popup = centered_rect(76, 16, area);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(catalog_confirmation_lines(app))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(" Confirm catalogs ")
                    .borders(Borders::ALL)
                    .padding(Padding::new(2, 2, 1, 1)),
            ),
        popup,
    );
}

fn catalog_confirmation_lines(app: &SkilledApp) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(
        "Confirm the resolved repository, roots, and compatible agents.",
    )];
    if let Some(preview) = app.pending_source() {
        let source = preview.inspected();
        lines.extend([
            Line::from(format!("Repository: {}", source.git_top_level().display())),
            Line::from(format!(
                "Branch: {}   HEAD: {}   {}",
                source.branch().unwrap_or("detached"),
                &source.head()[..source.head().len().min(8)],
                if source.dirty() { "dirty" } else { "clean" }
            )),
            Line::default(),
        ]);
        let capacity = 2;
        let start = visible_window_start(app.focused_catalog(), capacity);
        for (index, catalog) in preview
            .catalogs()
            .iter()
            .enumerate()
            .skip(start)
            .take(capacity)
        {
            lines.push(Line::from(format!(
                "{} [{}] {}  {:?}  C:{} X:{} O:{}",
                if index == app.focused_catalog() {
                    ">"
                } else {
                    " "
                },
                if catalog.included() { "x" } else { " " },
                catalog.relative_path().display(),
                catalog.classification(),
                yes_no(catalog.compatibility().claude_code()),
                yes_no(catalog.compatibility().codex()),
                yes_no(catalog.compatibility().opencode())
            )));
        }
    }
    if let Some(error) = app.source_error() {
        lines.push(Line::default());
        lines.push(Line::styled(
            error.to_owned(),
            Style::default().fg(Color::Red),
        ));
    }
    lines.extend([
        Line::default(),
        Line::from("Space include · c classification · 1/2/3 compatibility"),
        Line::from("Enter registers metadata only · Esc cancels"),
    ]);
    lines
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
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

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp) {
    let hints = if app.source_path_input_active() {
        " Type path   Enter Inspect   Esc Cancel   Ctrl-C Quit "
    } else if app.pending_source().is_some() {
        " j/k Move   Space Include   c Class   1/2/3 Agents   Enter Register   Esc Cancel "
    } else {
        match app.view() {
            View::Setup(SetupStep::DetectAgents) => {
                " j/k Move   Space Toggle   Enter Continue   Esc Back   q Quit "
            }
            View::Setup(SetupStep::DiscoverSources) => {
                " a Add source   Enter Continue   Esc Back   q Quit "
            }
            View::Setup(SetupStep::ConfirmCatalogs) => {
                " j/k Move   Space Include   c Class   1/2/3 Agents   Enter Register "
            }
            View::Setup(_) => " Enter Continue   Esc Back   q Quit ",
            View::Inventory => " 2 Sources   s Settings   ? Help   q Quit ",
            View::Sources => " Tab Pane   j/k Move   a Add source   1 Inventory   q Quit ",
            View::Settings => " Enter Rerun setup   Esc Close ",
        }
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
