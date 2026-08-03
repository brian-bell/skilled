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
                terminal_safe(source.label()),
                dirty_label(source.dirty())
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
    let mut metadata_lines = vec![
        Line::styled(
            terminal_safe(&source.git_top_level().display().to_string()),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::from(format!(
            "{} · {} · {} · scanned {}",
            terminal_safe(source.branch().unwrap_or("detached")),
            &source.head()[..source.head().len().min(8)],
            dirty_label(source.dirty()),
            source.last_scan_at()
        )),
    ];
    if let Some(remote) = source.remote_url() {
        metadata_lines.push(Line::from(format!("Remote: {}", terminal_safe(remote))));
    }
    if let Some(error) = source.source_error() {
        metadata_lines.push(Line::styled(
            format!("× Source unavailable — {}", terminal_safe(error)),
            Style::default().fg(Color::Red),
        ));
    } else {
        for catalog in source.catalogs() {
            if let Some(error) = catalog.scan_error() {
                metadata_lines.push(Line::styled(
                    format!(
                        "× {} — {}",
                        terminal_safe(&catalog.relative_path().display().to_string()),
                        terminal_safe(error)
                    ),
                    Style::default().fg(Color::Red),
                ));
            }
        }
    }
    metadata_lines.push(Line::default());
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
    let selected_details = variants
        .get(app.focused_variant())
        .map(|(catalog, selected)| {
            let mut details = vec![
                Line::default(),
                Line::from(match catalog.classification() {
                    CatalogClassification::Common => "Common catalog".to_owned(),
                    CatalogClassification::AgentSpecific => "Agent-specific catalog".to_owned(),
                }),
            ];
            let compatibility = catalog.compatibility();
            details.push(Line::from(format!(
                "Claude: {} · Codex: {} · OpenCode: {}",
                yes_no(compatibility.claude_code()),
                yes_no(compatibility.codex()),
                yes_no(compatibility.opencode())
            )));
            match selected.validation() {
                SkillValidation::Valid { description, .. } => {
                    details.push(Line::from(terminal_safe(description)));
                }
                SkillValidation::Invalid { message } => {
                    details.push(Line::styled(
                        terminal_safe(message),
                        Style::default().fg(Color::Red),
                    ));
                }
            }
            details
        })
        .unwrap_or_default();
    let wrap = Wrap { trim: false };
    let variant_lines = variants
        .iter()
        .enumerate()
        .map(|(index, (catalog, candidate))| {
            let status = if candidate.validation().is_valid() {
                "✓"
            } else {
                "×"
            };
            Line::from(format!(
                "{} {status} {}  ({})",
                if index == app.focused_variant() {
                    ">"
                } else {
                    " "
                },
                terminal_safe(candidate.directory_name()),
                terminal_safe(&catalog.relative_path().display().to_string())
            ))
        })
        .collect::<Vec<_>>();
    let viewport_height = usize::from(detail_inner.height);
    // Keep the complete focused row and then its details visible before giving wrapped metadata
    // the remaining rows. Oversized sections are clipped within their own viewport.
    let focused_variant_height = variant_lines
        .get(app.focused_variant())
        .map(|line| wrapped_line_count(line, detail_inner.width))
        .unwrap_or(0)
        .min(viewport_height);
    let selected_details_height = Paragraph::new(selected_details.clone())
        .wrap(wrap)
        .line_count(detail_inner.width)
        .min(viewport_height.saturating_sub(focused_variant_height));
    let height_after_details = viewport_height
        .saturating_sub(focused_variant_height)
        .saturating_sub(selected_details_height);
    let metadata_height = Paragraph::new(metadata_lines.clone())
        .wrap(wrap)
        .line_count(detail_inner.width)
        .min(height_after_details);
    let variants_height = viewport_height
        .saturating_sub(metadata_height)
        .saturating_sub(selected_details_height);
    let visible_variants = visible_wrapped_lines(
        &variant_lines,
        app.focused_variant(),
        detail_inner.width,
        variants_height,
    );
    let [metadata_area, variants_area, selected_details_area] = Layout::vertical([
        Constraint::Length(metadata_height as u16),
        Constraint::Length(variants_height as u16),
        Constraint::Length(selected_details_height as u16),
    ])
    .areas(detail_inner);
    frame.render_widget(Paragraph::new(metadata_lines).wrap(wrap), metadata_area);
    frame.render_widget(Paragraph::new(visible_variants).wrap(wrap), variants_area);
    frame.render_widget(
        Paragraph::new(selected_details).wrap(wrap),
        selected_details_area,
    );
}

fn wrapped_line_count(line: &Line<'_>, width: u16) -> usize {
    Paragraph::new(line.clone())
        .wrap(Wrap { trim: false })
        .line_count(width)
}

fn visible_wrapped_lines(
    lines: &[Line<'static>],
    focused: usize,
    width: u16,
    height: usize,
) -> Vec<Line<'static>> {
    let Some(focused_line) = lines.get(focused) else {
        return Vec::new();
    };
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let mut start = focused;
    let mut used_rows = wrapped_line_count(focused_line, width);
    while start > 0 {
        let previous_rows = wrapped_line_count(&lines[start - 1], width);
        if used_rows.saturating_add(previous_rows) > height {
            break;
        }
        start -= 1;
        used_rows = used_rows.saturating_add(previous_rows);
    }
    let mut visible = Vec::new();
    let mut used_rows = 0_usize;
    for line in &lines[start..] {
        let rows = wrapped_line_count(line, width);
        if !visible.is_empty() && used_rows.saturating_add(rows) > height {
            break;
        }
        visible.push(line.clone());
        used_rows = used_rows.saturating_add(rows);
        if used_rows >= height {
            break;
        }
    }
    visible
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
            terminal_safe(error),
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
    let block = Block::default()
        .title(" Confirm catalogs ")
        .borders(Borders::ALL)
        .padding(Padding::new(2, 2, 1, 1));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let (metadata, catalogs, error, footer) = catalog_confirmation_sections(app);
    let wrap = Wrap { trim: false };
    let viewport_height = usize::from(inner.height);
    let footer_height = Paragraph::new(footer.clone())
        .wrap(wrap)
        .line_count(inner.width)
        .min(viewport_height);
    let error_height = Paragraph::new(error.clone())
        .wrap(wrap)
        .line_count(inner.width)
        .min(viewport_height.saturating_sub(footer_height));
    let focused_height = catalogs
        .get(app.focused_catalog())
        .map(|line| wrapped_line_count(line, inner.width))
        .unwrap_or(0)
        .min(
            viewport_height
                .saturating_sub(footer_height)
                .saturating_sub(error_height),
        );
    let metadata_height = Paragraph::new(metadata.clone())
        .wrap(wrap)
        .line_count(inner.width)
        .min(
            viewport_height
                .saturating_sub(footer_height)
                .saturating_sub(error_height)
                .saturating_sub(focused_height),
        );
    let catalog_height = viewport_height
        .saturating_sub(metadata_height)
        .saturating_sub(error_height)
        .saturating_sub(footer_height);
    let visible_catalogs = visible_wrapped_lines(
        &catalogs,
        app.focused_catalog(),
        inner.width,
        catalog_height,
    );
    let visible_catalog_height = Paragraph::new(visible_catalogs.clone())
        .wrap(wrap)
        .line_count(inner.width)
        .min(catalog_height);
    let mut spare_rows = viewport_height
        .saturating_sub(metadata_height)
        .saturating_sub(visible_catalog_height)
        .saturating_sub(error_height)
        .saturating_sub(footer_height);
    let metadata_gap = !metadata.is_empty() && !visible_catalogs.is_empty() && spare_rows > 0;
    spare_rows = spare_rows.saturating_sub(usize::from(metadata_gap));
    let error_gap = !error.is_empty() && spare_rows > 0;
    spare_rows = spare_rows.saturating_sub(usize::from(error_gap));
    let footer_gap = !footer.is_empty() && spare_rows > 0;

    let mut y = inner.y;
    render_confirmation_section(frame, inner, &mut y, metadata_height, metadata);
    y = y.saturating_add(u16::from(metadata_gap));
    render_confirmation_section(
        frame,
        inner,
        &mut y,
        visible_catalog_height,
        visible_catalogs,
    );
    y = y.saturating_add(u16::from(error_gap));
    render_confirmation_section(frame, inner, &mut y, error_height, error);
    y = y.saturating_add(u16::from(footer_gap));
    render_confirmation_section(frame, inner, &mut y, footer_height, footer);
}

fn catalog_confirmation_lines(app: &SkilledApp) -> Vec<Line<'static>> {
    let (mut lines, catalogs, error, footer) = catalog_confirmation_sections(app);
    lines.push(Line::default());
    let capacity = 2;
    let start = visible_window_start(app.focused_catalog(), capacity);
    lines.extend(catalogs.into_iter().skip(start).take(capacity));
    if !error.is_empty() {
        lines.push(Line::default());
        lines.extend(error);
    }
    lines.push(Line::default());
    lines.extend(footer);
    lines
}

fn catalog_confirmation_sections(
    app: &SkilledApp,
) -> (
    Vec<Line<'static>>,
    Vec<Line<'static>>,
    Vec<Line<'static>>,
    Vec<Line<'static>>,
) {
    let mut metadata = vec![Line::from(
        "Confirm the resolved repository, roots, and compatible agents.",
    )];
    let mut catalogs = Vec::new();
    if let Some(preview) = app.pending_source() {
        let source = preview.inspected();
        metadata.extend([
            Line::from(format!(
                "Repository: {}",
                terminal_safe(&source.git_top_level().display().to_string())
            )),
            Line::from(format!(
                "Branch: {}   HEAD: {}   {}",
                terminal_safe(source.branch().unwrap_or("detached")),
                &source.head()[..source.head().len().min(8)],
                dirty_label(source.dirty())
            )),
        ]);
        for (index, catalog) in preview.catalogs().iter().enumerate() {
            catalogs.push(Line::from(format!(
                "{} [{}] {}  {:?}  C:{} X:{} O:{}",
                if index == app.focused_catalog() {
                    ">"
                } else {
                    " "
                },
                if catalog.included() { "x" } else { " " },
                terminal_safe(&catalog.relative_path().display().to_string()),
                catalog.classification(),
                yes_no(catalog.compatibility().claude_code()),
                yes_no(catalog.compatibility().codex()),
                yes_no(catalog.compatibility().opencode())
            )));
        }
    }
    let mut error = Vec::new();
    if let Some(message) = app.source_error() {
        error.push(Line::styled(
            terminal_safe(message),
            Style::default().fg(Color::Red),
        ));
    }
    let footer = vec![
        Line::from("Space include · c classification · 1/2/3 compatibility"),
        Line::from("Enter registers metadata only · Esc cancels"),
    ];
    (metadata, catalogs, error, footer)
}

fn render_confirmation_section(
    frame: &mut Frame<'_>,
    inner: Rect,
    y: &mut u16,
    height: usize,
    lines: Vec<Line<'static>>,
) {
    let height = u16::try_from(height).unwrap_or(u16::MAX);
    if height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        Rect::new(inner.x, *y, inner.width, height),
    );
    *y = y.saturating_add(height);
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn dirty_label(dirty: Option<bool>) -> &'static str {
    match dirty {
        Some(true) => "dirty",
        Some(false) => "clean",
        None => "status unavailable",
    }
}

fn terminal_safe(value: &str) -> String {
    let mut safe = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control() {
            safe.extend(character.escape_default());
        } else {
            safe.push(character);
        }
    }
    safe
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
