use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Margin, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap},
};

use crate::{
    SetupStep, SkilledApp, SourcesPane, View,
    components::{self, KeyHint},
    source::{
        CatalogClassification, CatalogProposal, RegisteredSource, SkillCandidate, SkillValidation,
    },
    theme::{self, Tone},
    viewport,
};

pub const MINIMUM_WIDTH: u16 = 80;
pub const MINIMUM_HEIGHT: u16 = 24;

pub fn render(frame: &mut Frame<'_>, app: &SkilledApp) {
    let area = frame.area();
    if area.width < MINIMUM_WIDTH || area.height < MINIMUM_HEIGHT {
        render_size_notice(frame, area);
        return;
    }

    frame.render_widget(Block::new().style(theme::app_surface()), area);

    let [title_bar, navigation, workspace, key_hints] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);

    render_title_bar(frame, title_bar, app);
    render_navigation(frame, navigation, app);
    let body = workspace;
    match app.view() {
        View::Setup(step) => render_setup(frame, body, app, step),
        View::Inventory => render_inventory(frame, body),
        View::Sources => render_sources(frame, body, app),
        View::Settings => {
            render_inventory(frame, body);
            render_settings(frame, body);
        }
    }
    if app.source_path_input_active() {
        render_source_path_entry(frame, area, app);
    } else if app.pending_source().is_some() && app.view() == View::Sources {
        render_catalog_confirmation(frame, area, app);
    }
    if let Some(context) = app.help_context() {
        render_help(frame, area, context, app);
    }
    render_footer(frame, key_hints, app);
}

fn render_title_bar(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp) {
    // The prototype places session state beside the navigation tabs. At eighty
    // columns the tab strip already fills that row, so the status shares the
    // title bar instead of competing with navigation for space.
    //
    // The two halves get their own rectangles because a Paragraph repaints its
    // whole area before drawing: rendering the status across the full row would
    // silently flatten the product mark and wordmark to the status colour.
    let status = SessionStatus::of(app);
    let label = status.label();
    let status_width = u16::try_from(Span::raw(&label).width() + 3)
        .unwrap_or(u16::MAX)
        .min(area.width);
    let [product, session] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(status_width)]).areas(area);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ◆ ", theme::product_mark()),
            Span::styled("skilled", theme::product_name()),
            Span::styled("   global", theme::chrome()),
        ])),
        product,
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("● ", theme::tone_style(status.tone())),
            Span::styled(label, theme::chrome()),
            Span::raw(" "),
        ]))
        .alignment(Alignment::Right),
        session,
    );
}

/// What the application can honestly say about the current session.
///
/// Skilled performs no installation scan and no network access in this release,
/// so the status may only describe setup progress and registered-source counts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionStatus {
    SetupInProgress,
    Ready { sources: usize },
}

impl SessionStatus {
    fn of(app: &SkilledApp) -> Self {
        match app.view() {
            View::Setup(_) => Self::SetupInProgress,
            _ => Self::Ready {
                sources: app.sources().len(),
            },
        }
    }

    fn tone(self) -> Tone {
        match self {
            Self::SetupInProgress => Tone::Warning,
            Self::Ready { .. } => Tone::Healthy,
        }
    }

    fn label(self) -> String {
        match self {
            Self::SetupInProgress => "setup in progress".to_owned(),
            Self::Ready { sources: 1 } => "ready · 1 source registered".to_owned(),
            Self::Ready { sources } => format!("ready · {sources} sources registered"),
        }
    }
}

fn render_navigation(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp) {
    frame.render_widget(Block::new().style(theme::nav_surface()), area);

    if let Some((owner, note)) = keyboard_owner(app) {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!(" {owner} "), theme::nav_active()),
                Span::styled(format!("  {note}"), theme::nav_disabled()),
            ])),
            area,
        );
        return;
    }

    let mut spans = Vec::new();
    for destination in Destination::ALL {
        let active = destination.is_active(app.view());
        let style = match (destination.is_available(), active) {
            (false, _) => theme::nav_disabled(),
            (true, true) => theme::nav_active(),
            (true, false) => theme::nav_inactive(),
        };
        spans.push(Span::styled(
            if active {
                components::FOCUS_MARKER
            } else {
                " "
            },
            style,
        ));
        // The digit is the route, so it appears only where pressing it works:
        // never for a destination this release cannot open, and never for the
        // view already on screen.
        let key = match (destination.is_available(), active) {
            (true, false) => format!("{} ", destination.key()),
            _ => String::new(),
        };
        spans.push(Span::styled(
            format!(
                "{key}{}{} ",
                destination.title(),
                if destination.is_available() {
                    ""
                } else {
                    " (soon)"
                }
            ),
            style,
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The context that currently owns the keyboard, if it is not the tab strip.
///
/// Setup and every dialog rebind or ignore the destination keys, so the strip
/// would advertise routes that do not work — while a catalog confirmation is
/// open, for instance, `1` and `2` toggle agent compatibility. The row names
/// the owner instead.
///
/// The label must match what is actually on screen, and a pending source
/// outside the Sources view is rendered inline rather than as a dialog.
///
/// The note states the present fact rather than predicting when the lock
/// lifts. Confirming a path opens the catalog confirmation and confirming
/// Settings starts setup, so "unlocks when this dialog closes" would be a
/// promise the next transition breaks.
fn keyboard_owner(app: &SkilledApp) -> Option<(String, &'static str)> {
    const SETUP_NOTE: &str = "navigation is locked during setup";
    const DIALOG_NOTE: &str = "navigation is locked while this dialog is open";

    if app.help_context().is_some() {
        return Some(("Keyboard reference".to_owned(), DIALOG_NOTE));
    }

    let in_setup = matches!(app.view(), View::Setup(_));
    let note = if in_setup { SETUP_NOTE } else { DIALOG_NOTE };

    if app.source_path_input_active() {
        return Some(("Add source".to_owned(), note));
    }
    // Mirrors the render gate: the confirmation is only a dialog in Sources.
    if app.pending_source().is_some() && app.view() == View::Sources {
        return Some(("Confirm catalogs".to_owned(), note));
    }
    match app.view() {
        View::Setup(step) => Some((format!("Setup · {}", step.title()), note)),
        View::Settings => Some(("Settings".to_owned(), note)),
        View::Inventory | View::Sources => None,
    }
}

/// A primary destination in the persistent navigation bar.
///
/// Destinations without an implementation in this release are still listed so
/// the navigation model is honest about what the product will offer, but they
/// carry no count, no route, and no key binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Destination {
    Inventory,
    Sources,
    Updates,
    Doctor,
}

impl Destination {
    const ALL: [Self; 4] = [Self::Inventory, Self::Sources, Self::Updates, Self::Doctor];

    fn key(self) -> char {
        match self {
            Self::Inventory => '1',
            Self::Sources => '2',
            Self::Updates => '3',
            Self::Doctor => '4',
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Inventory => "Inventory",
            Self::Sources => "Sources",
            Self::Updates => "Updates",
            Self::Doctor => "Doctor",
        }
    }

    fn is_available(self) -> bool {
        matches!(self, Self::Inventory | Self::Sources)
    }

    fn is_active(self, view: View) -> bool {
        match self {
            Self::Inventory => matches!(view, View::Inventory | View::Settings),
            Self::Sources => view == View::Sources,
            Self::Updates | Self::Doctor => false,
        }
    }
}

fn render_setup(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp, step: SetupStep) {
    const STEP_COUNT: usize = 7;
    let (width, height) = match viewport::classify(area) {
        viewport::Viewport::Compact => (76, 21),
        viewport::Viewport::Wide => (92, 28),
    };
    let popup = centered_rect(width, height, area);
    frame.render_widget(Clear, popup);
    let block = components::dialog_frame("First-run setup", "global skills only");
    let regions = components::dialog_regions(block.inner(popup), 31);
    frame.render_widget(block, popup);

    let [heading, content] =
        Layout::vertical([Constraint::Length(4), Constraint::Min(0)]).areas(regions.body);
    frame.render_widget(
        Paragraph::new(vec![
            components::segmented_progress(step.number(), STEP_COUNT, heading.width),
            Line::styled(
                format!("STEP {} / {STEP_COUNT}", step.number()),
                theme::section_title(),
            ),
            Line::styled(step.title(), theme::section_title()),
        ]),
        heading,
    );
    if step == SetupStep::ConfirmCatalogs && app.pending_source().is_some() {
        render_catalog_confirmation_content(frame, content, app);
    } else {
        frame.render_widget(
            Paragraph::new(setup_lines(app, step, content.width)).wrap(Wrap { trim: false }),
            content,
        );
    }
    frame.render_widget(
        Paragraph::new(components::rule(regions.divider.width)),
        regions.divider,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            if step == SetupStep::ConfirmCatalogs && app.pending_source().is_some() {
                "Registration records metadata only"
            } else {
                "Setup is persisted when it finishes"
            },
            theme::key_label(),
        ))),
        regions.status,
    );
    frame.render_widget(
        Paragraph::new(setup_action_line(step, app.pending_source().is_some()).right_aligned()),
        regions.actions,
    );
}

fn setup_action_line(step: SetupStep, pending_source: bool) -> Line<'static> {
    if step == SetupStep::ConfirmCatalogs && pending_source {
        return Line::from(vec![
            Span::styled("Esc", theme::key_cap()),
            Span::raw(" "),
            Span::styled("Cancel", theme::key_label()),
            Span::raw("   "),
            Span::styled("Enter", theme::key_cap()),
            Span::raw(" "),
            Span::styled("Register", theme::key_label()),
        ]);
    }
    let mut spans = Vec::new();
    if step != SetupStep::Welcome {
        spans.extend([
            Span::styled("Esc", theme::key_cap()),
            Span::raw(" "),
            Span::styled("Back", theme::key_label()),
            Span::raw("   "),
        ]);
    }
    spans.extend([
        Span::styled("Enter", theme::key_cap()),
        Span::raw(" "),
        Span::styled(
            if step == SetupStep::Summary {
                "Inventory"
            } else {
                "Continue"
            },
            theme::key_label(),
        ),
    ]);
    Line::from(spans)
}

fn setup_lines(app: &SkilledApp, step: SetupStep, width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match step {
        SetupStep::Welcome => {
            lines.extend([
                Line::from("Skilled manages global skills for Claude Code, Codex, and OpenCode."),
                Line::from("This setup configures agents and local source metadata."),
                Line::default(),
                Line::from("No coding agent is launched during setup or diagnosis."),
                Line::from("Existing physical files, directories, and unknown links are never overwritten."),
            ]);
        }
        SetupStep::DetectAgents => {
            lines.push(Line::from("Choose the agents Skilled should configure."));
            lines.push(Line::default());
            for (index, detection) in app.agents().iter().enumerate() {
                let root = if detection.root_exists() {
                    components::badge(Tone::Healthy, "root found")
                } else {
                    components::badge(Tone::Inactive, "root not found")
                };
                let executable = if detection.executable_path().is_some() {
                    components::badge(Tone::Healthy, "executable found")
                } else {
                    components::badge(Tone::Inactive, "executable not found")
                };
                lines.push(components::list_row(
                    vec![
                        Span::raw(format!(
                            "[{}] {:<11}  ",
                            if detection.selected() { "x" } else { " " },
                            detection.kind().display_name(),
                        )),
                        root,
                        Span::raw("   "),
                        executable,
                    ],
                    index == app.focused_agent(),
                    width,
                ));
            }
        }
        SetupStep::ChooseScanRoots => lines.extend([
            Line::from(components::badge(
                Tone::Inactive,
                "scan roots not configured",
            )),
            Line::default(),
            Line::from("Skilled has not scanned your home directory."),
            Line::from("Continue without scanning; add a known checkout in Discover sources."),
        ]),
        SetupStep::DiscoverSources => lines.extend([
            Line::from(components::badge(
                Tone::Inactive,
                "automatic discovery unavailable",
            )),
            Line::default(),
            Line::from(format!("Registered sources: {}", app.sources().len())),
            Line::from("Press a to inspect a known local Git checkout."),
            Line::from("Skilled never scans the entire home directory by default."),
        ]),
        SetupStep::ConfirmCatalogs => lines.extend([
            Line::from(components::badge(
                Tone::Inactive,
                "no catalogs awaiting confirmation",
            )),
            Line::default(),
            Line::from("Catalog confirmation follows inspection of a local source."),
            Line::from("Registration records metadata only; it does not install skills."),
        ]),
        SetupStep::ScanInstallations => lines.extend([
            Line::from(components::badge(
                Tone::Inactive,
                "installation roots not scanned",
            )),
            Line::default(),
            Line::from("This build cannot report installation or Doctor status."),
            Line::from("Continue without reading or changing any agent skill root."),
        ]),
        SetupStep::Summary => lines.extend([
            Line::from("Setup is ready to finish."),
            Line::default(),
            Line::from(format!(
                "Configured agents: {}",
                app.agents().iter().filter(|agent| agent.selected()).count()
            )),
            Line::from(format!(
                "Sources: {}   Skills: {}",
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
    let (primary, detail) = viewport::workspace_regions(area);
    if let Some(detail) = detail {
        render_inventory_detail(frame, detail);
    }

    let [header, body] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(primary);
    frame.render_widget(
        Paragraph::new(vec![
            components::pane_header("Global inventory", "not scanned"),
            components::rule(header.width),
        ]),
        header,
    );

    // Nothing scans installation roots yet, so the only truthful inventory is
    // an empty one. The copy points at the work the release does support.
    let body = body.inner(Margin {
        horizontal: 2,
        vertical: 0,
    });
    frame.render_widget(
        components::empty_state(
            "⌕",
            "Installation roots have not been scanned",
            "Skilled has not looked at any installation root yet, so it cannot \
             say what is installed. Register a local source in Sources to \
             prepare for installation in a later release.",
            body,
        ),
        body,
    );
}

/// The detail region beside a wide Inventory.
///
/// There is nothing to select yet, so the region explains what it will show
/// rather than standing empty or inventing a subject.
fn render_inventory_detail(frame: &mut Frame<'_>, area: Rect) {
    let [separator, region] =
        Layout::horizontal([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled("│", theme::rule()));
            usize::from(separator.height)
        ]),
        separator,
    );

    // The header mirrors the primary pane's so both regions measure their
    // centred content against the same remaining height.
    let region = region.inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    let [header, body] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(region);
    frame.render_widget(
        Paragraph::new(vec![
            components::pane_header("Details", "no selection"),
            components::rule(header.width),
        ]),
        header,
    );
    frame.render_widget(
        components::empty_state(
            "·",
            "Nothing to show",
            "Identity, provenance, and installation paths appear here once \
             installation inventory exists.",
            body,
        ),
        body,
    );
}

fn render_sources(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp) {
    match viewport::classify(area) {
        viewport::Viewport::Wide => {
            let (primary, details) = viewport::workspace_regions(area);
            let details = details.expect("wide Sources workspace has a detail region");
            let [repositories, _, variants] = Layout::horizontal([
                Constraint::Percentage(42),
                Constraint::Length(1),
                Constraint::Min(1),
            ])
            .areas(primary);
            render_source_repositories(frame, repositories, app);
            render_source_variants(frame, variants, app);
            render_source_details(frame, details, app);
        }
        viewport::Viewport::Compact => match app.sources_pane() {
            SourcesPane::Repositories => render_source_repositories(frame, area, app),
            SourcesPane::Variants => render_source_variants(frame, area, app),
            SourcesPane::Details => render_source_details(frame, area, app),
        },
    }
}

fn source_region_block(
    heading: &str,
    subtitle: &str,
    pane: SourcesPane,
    app: &SkilledApp,
) -> Block<'static> {
    let focused = app.sources_pane() == pane;
    Block::default()
        .title(components::focused_pane_header(heading, subtitle, focused))
        .borders(Borders::ALL)
        .border_style(theme::pane_border(focused))
        .padding(Padding::horizontal(1))
}

fn render_source_repositories(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp) {
    let block = source_region_block(
        "Repositories",
        &format!("{} registered", app.sources().len()),
        SourcesPane::Repositories,
        app,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.sources().is_empty() {
        frame.render_widget(
            components::empty_state(
                "·",
                "No sources are registered",
                "Press a to register a local Git checkout.",
                inner,
            ),
            inner,
        );
        return;
    }

    let capacity = usize::from(inner.height.max(1));
    let start = visible_window_start(app.focused_source(), capacity);
    let lines = app
        .sources()
        .iter()
        .enumerate()
        .skip(start)
        .take(capacity)
        .map(|(index, source)| {
            components::list_row(
                vec![
                    Span::raw(format!("{}  ", terminal_safe(source.label()))),
                    source_status_badge(source),
                ],
                index == app.focused_source(),
                inner.width,
            )
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_source_variants(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp) {
    let variants = app
        .selected_source()
        .map(flattened_variants)
        .unwrap_or_default();
    let subtitle = match app.selected_source() {
        Some(source) if source.source_error().is_some() => "unavailable".to_owned(),
        Some(_) => format!("{} found", variants.len()),
        None => "no source".to_owned(),
    };
    let block = source_region_block("Available variants", &subtitle, SourcesPane::Variants, app);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(source) = app.selected_source() else {
        frame.render_widget(
            components::empty_state(
                "·",
                "No source selected",
                "Press a to register a local Git checkout.",
                inner,
            ),
            inner,
        );
        return;
    };

    if source.source_error().is_some() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(components::badge(Tone::Critical, "unavailable")),
                Line::from("Open Details for the source error."),
            ])
            .wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }

    let catalog_errors = source
        .catalogs()
        .iter()
        .filter_map(|catalog| {
            catalog.scan_error().map(|error| {
                Line::from(vec![
                    components::badge(Tone::Critical, "unavailable"),
                    Span::raw(format!(
                        " {}: {}",
                        terminal_safe(&catalog.relative_path().display().to_string()),
                        terminal_safe(error)
                    )),
                ])
            })
        })
        .collect::<Vec<_>>();

    if variants.is_empty() {
        if catalog_errors.is_empty() {
            frame.render_widget(
                components::empty_state(
                    "·",
                    "No variants found",
                    "The selected source contains no immediate skill definitions.",
                    inner,
                ),
                inner,
            );
        } else {
            frame.render_widget(
                Paragraph::new(
                    catalog_errors
                        .into_iter()
                        .chain([Line::from("Open Details for the catalog error.")])
                        .collect::<Vec<_>>(),
                )
                .wrap(Wrap { trim: false }),
                inner,
            );
        }
        return;
    }

    let mut lines = catalog_errors;
    lines.extend(variants.iter().enumerate().map(|(index, variant)| {
        let valid = variant.candidate.validation().is_valid();
        components::list_row(
            vec![
                components::badge(
                    if valid { Tone::Healthy } else { Tone::Critical },
                    if valid { "valid" } else { "invalid" },
                ),
                Span::raw(format!(
                    " {}  ({})",
                    terminal_safe(variant.candidate.directory_name()),
                    terminal_safe(&variant.candidate.relative_path().display().to_string())
                )),
            ],
            index == app.focused_variant(),
            inner.width,
        )
    }));
    let error_count = source
        .catalogs()
        .iter()
        .filter(|catalog| catalog.scan_error().is_some())
        .count();
    let focused_line = error_count.saturating_add(app.focused_variant());
    let visible =
        visible_wrapped_lines(&lines, focused_line, inner.width, usize::from(inner.height));
    frame.render_widget(Paragraph::new(visible).wrap(Wrap { trim: false }), inner);
}

fn render_source_details(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp) {
    let selected = selected_variant(app);
    let subtitle = selected
        .map(|variant| terminal_safe(variant.candidate.directory_name()))
        .or_else(|| {
            app.selected_source()
                .map(|source| terminal_safe(source.label()))
        })
        .unwrap_or_else(|| "no selection".to_owned());
    let block = source_region_block("Details", &subtitle, SourcesPane::Details, app);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(source) = app.selected_source() else {
        frame.render_widget(
            components::empty_state(
                "·",
                "No source selected",
                "Select a repository to see its stored details.",
                inner,
            ),
            inner,
        );
        return;
    };

    let mut lines = Vec::new();
    push_detail_section(&mut lines, "REPOSITORY", inner.width);
    lines.push(detail_field("Label", source.label()));
    lines.push(detail_field_bounded(
        "Path",
        &source.git_top_level().display().to_string(),
        inner.width,
        2,
    ));
    lines.push(Line::from(vec![
        Span::styled("Branch: ", theme::pane_subtitle()),
        Span::raw(terminal_safe(source.branch().unwrap_or("detached"))),
        Span::raw(" · "),
        Span::styled("Status: ", theme::pane_subtitle()),
        source_status_badge(source),
    ]));
    lines.push(detail_field("HEAD", source.head()));
    lines.push(detail_field_bounded(
        "Remote",
        source.remote_url().unwrap_or("not configured"),
        inner.width,
        2,
    ));
    lines.push(detail_field(
        "Last scan",
        &source.last_scan_at().to_string(),
    ));
    if let Some(error) = source.source_error() {
        lines.push(detail_field("Source error", error));
    }

    push_detail_section(&mut lines, "CATALOG", inner.width);
    if let Some(variant) = selected {
        lines.push(Line::from(vec![
            Span::styled("Path: ", theme::pane_subtitle()),
            Span::raw(terminal_safe(
                &variant.catalog.relative_path().display().to_string(),
            )),
            Span::raw(" · "),
            Span::styled("Classification: ", theme::pane_subtitle()),
            Span::raw(catalog_classification(variant.catalog)),
        ]));
        let compatibility = variant.catalog.compatibility();
        lines.push(detail_field(
            "Compatibility",
            &format!(
                "Claude Code: {} · Codex: {} · OpenCode: {}",
                yes_no(compatibility.claude_code()),
                yes_no(compatibility.codex()),
                yes_no(compatibility.opencode())
            ),
        ));
        if let Some(error) = variant.catalog.scan_error() {
            lines.push(detail_field("Catalog error", error));
        }
    } else {
        lines.push(Line::from(
            "No variant selected; catalog metadata is unavailable.",
        ));
        for catalog in source.catalogs() {
            if let Some(error) = catalog.scan_error() {
                lines.push(detail_field(
                    &format!(
                        "Catalog error ({})",
                        terminal_safe(&catalog.relative_path().display().to_string())
                    ),
                    error,
                ));
            }
        }
    }

    push_detail_section(&mut lines, "VARIANT", inner.width);
    if let Some(variant) = selected {
        let (status, name) = match variant.candidate.validation() {
            SkillValidation::Valid { name, .. } => {
                (components::badge(Tone::Healthy, "valid"), name.as_str())
            }
            SkillValidation::Invalid { .. } => (
                components::badge(Tone::Critical, "invalid"),
                variant.candidate.directory_name(),
            ),
        };
        lines.push(Line::from(vec![
            Span::styled("Directory: ", theme::pane_subtitle()),
            Span::raw(terminal_safe(variant.candidate.directory_name())),
            Span::raw(" · "),
            Span::styled("Name: ", theme::pane_subtitle()),
            Span::raw(terminal_safe(name)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Path: ", theme::pane_subtitle()),
            Span::raw(terminal_safe(
                &variant.candidate.relative_path().display().to_string(),
            )),
            Span::raw(" · "),
            Span::styled("Status: ", theme::pane_subtitle()),
            status,
        ]));
        match variant.candidate.validation() {
            SkillValidation::Valid { description, .. } => {
                lines.push(detail_field("Description", description));
            }
            SkillValidation::Invalid { message } => {
                lines.push(detail_field("Validation error", message));
            }
        }
    } else {
        lines.push(Line::from("No variant selected."));
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

#[derive(Clone, Copy)]
struct SourceVariant<'a> {
    catalog: &'a CatalogProposal,
    candidate: &'a SkillCandidate,
}

fn flattened_variants(source: &RegisteredSource) -> Vec<SourceVariant<'_>> {
    source
        .catalogs()
        .iter()
        .flat_map(|catalog| {
            catalog
                .candidates()
                .iter()
                .map(move |candidate| SourceVariant { catalog, candidate })
        })
        .collect()
}

fn selected_variant(app: &SkilledApp) -> Option<SourceVariant<'_>> {
    app.selected_source()
        .map(flattened_variants)
        .and_then(|variants| variants.into_iter().nth(app.focused_variant()))
}

fn catalog_classification(catalog: &CatalogProposal) -> &'static str {
    match catalog.classification() {
        CatalogClassification::Common => "Common",
        CatalogClassification::AgentSpecific => "Agent-specific",
    }
}

fn source_status_badge(source: &RegisteredSource) -> Span<'static> {
    if source.source_error().is_some() {
        components::badge(Tone::Critical, "unavailable")
    } else {
        worktree_badge(source.dirty())
    }
}

fn push_detail_section(lines: &mut Vec<Line<'static>>, title: &str, width: u16) {
    lines.push(Line::styled(title.to_owned(), theme::section_title()));
    lines.push(components::rule(width));
}

fn detail_field(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}: "), theme::pane_subtitle()),
        Span::raw(terminal_safe(value)),
    ])
}

fn detail_field_bounded(
    label: &str,
    value: &str,
    width: u16,
    maximum_lines: usize,
) -> Line<'static> {
    let safe = terminal_safe(value);
    let label_width = Span::raw(format!("{label}: ")).width();
    let budget = usize::from(width)
        .saturating_mul(maximum_lines)
        .saturating_sub(label_width);
    if Span::raw(&safe).width() <= budget {
        return detail_field(label, &safe);
    }

    const ELLIPSIS: &str = "...";
    let mut bounded = String::new();
    for character in safe.chars() {
        let mut candidate = bounded.clone();
        candidate.push(character);
        candidate.push_str(ELLIPSIS);
        if Span::raw(&candidate).width() > budget {
            break;
        }
        bounded.push(character);
    }
    bounded.push_str(ELLIPSIS);
    detail_field(label, &bounded)
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
        lines.push(Line::from(components::badge(
            Tone::Critical,
            &terminal_safe(error),
        )));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(components::dialog_frame("Add source", "local Git checkout")),
        popup,
    );
}

fn render_catalog_confirmation(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp) {
    let popup = centered_rect(76, 16, area);
    frame.render_widget(Clear, popup);
    let block = components::dialog_frame("Confirm catalogs", "registration only");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    render_catalog_confirmation_content(frame, inner, app);
}

fn render_catalog_confirmation_content(frame: &mut Frame<'_>, inner: Rect, app: &SkilledApp) {
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
            Line::from(vec![
                Span::raw(format!(
                    "Branch: {}   HEAD: {}   ",
                    terminal_safe(source.branch().unwrap_or("detached")),
                    &source.head()[..source.head().len().min(8)]
                )),
                worktree_badge(source.dirty()),
            ]),
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
        error.push(Line::from(components::badge(
            Tone::Critical,
            &terminal_safe(message),
        )));
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

/// The worktree state of a registered source.
///
/// An unavailable status is its own tone: Skilled did not observe a clean tree,
/// and must not imply that it did.
fn worktree_badge(dirty: Option<bool>) -> Span<'static> {
    match dirty {
        Some(true) => components::badge(Tone::Warning, "dirty"),
        Some(false) => components::badge(Tone::Healthy, "clean"),
        None => components::badge(Tone::Inactive, "status unavailable"),
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
    frame.render_widget(Clear, area);
    frame.render_widget(Block::new().style(theme::app_surface()), area);
    let width = match viewport::classify(area) {
        viewport::Viewport::Compact => 68,
        viewport::Viewport::Wide => 72,
    };
    let popup = centered_rect(width, 14, area);
    frame.render_widget(Clear, popup);
    let block = components::dialog_frame("Settings", "global scope");
    let regions = components::dialog_regions(block.inner(popup), 29);
    frame.render_widget(block, popup);
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled("Setup", theme::section_title()),
            Line::default(),
            components::list_row(vec![Span::raw("Rerun setup")], true, regions.body.width),
            Line::default(),
            Line::from("Reset setup completion and return to Welcome."),
            Line::from("Agent root and executable detection is refreshed."),
            Line::from("Agent selections and registered sources are retained."),
            Line::from("Enter reruns setup; Esc closes Settings."),
        ])
        .wrap(Wrap { trim: false }),
        regions.body,
    );
    frame.render_widget(
        Paragraph::new(components::rule(regions.divider.width)),
        regions.divider,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "No agent is launched",
            theme::key_label(),
        ))),
        regions.status,
    );
    frame.render_widget(
        Paragraph::new(
            Line::from(vec![
                Span::styled("Enter", theme::key_cap()),
                Span::raw(" "),
                Span::styled("Rerun", theme::key_label()),
                Span::raw("   "),
                Span::styled("Esc", theme::key_cap()),
                Span::raw(" "),
                Span::styled("Close", theme::key_label()),
            ])
            .right_aligned(),
        ),
        regions.actions,
    );
}

fn render_help(frame: &mut Frame<'_>, area: Rect, context: View, app: &SkilledApp) {
    let viewport = viewport::classify(area);
    let (width, height) = match viewport {
        viewport::Viewport::Compact => (76, 18),
        viewport::Viewport::Wide => (96, 20),
    };
    let popup = centered_rect(width, height, area);
    frame.render_widget(Clear, popup);
    let scope = help_scope(context);
    let block = components::dialog_frame("Keyboard reference", &scope);
    let regions = components::dialog_regions(block.inner(popup), 11);
    frame.render_widget(block, popup);
    let commands = help_commands(context, app);
    match viewport {
        viewport::Viewport::Compact => {
            let mut lines = vec![
                Line::from("Only commands available in this context are listed."),
                Line::default(),
            ];
            lines.extend(commands.iter().map(help_command_line));
            frame.render_widget(
                Paragraph::new(lines).wrap(Wrap { trim: false }),
                regions.body,
            );
        }
        viewport::Viewport::Wide => {
            let [intro, command_area] =
                Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(regions.body);
            frame.render_widget(
                Paragraph::new("Only commands available in this context are listed."),
                intro,
            );
            let [left, right] =
                Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .spacing(2)
                    .areas(command_area);
            let midpoint = commands.len().div_ceil(2);
            frame.render_widget(
                Paragraph::new(
                    commands[..midpoint]
                        .iter()
                        .map(help_command_line)
                        .collect::<Vec<_>>(),
                )
                .wrap(Wrap { trim: false }),
                left,
            );
            frame.render_widget(
                Paragraph::new(
                    commands[midpoint..]
                        .iter()
                        .map(help_command_line)
                        .collect::<Vec<_>>(),
                )
                .wrap(Wrap { trim: false }),
                right,
            );
        }
    }
    frame.render_widget(
        Paragraph::new(components::rule(regions.divider.width)),
        regions.divider,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("Commands for {scope}"),
            theme::key_label(),
        ))),
        regions.status,
    );
    frame.render_widget(
        Paragraph::new(
            Line::from(vec![
                Span::styled("Esc", theme::key_cap()),
                Span::raw(" "),
                Span::styled("Close", theme::key_label()),
            ])
            .right_aligned(),
        ),
        regions.actions,
    );
}

fn help_command_line(command: &HelpCommand) -> Line<'static> {
    Line::from(vec![
        Span::styled(command.key, theme::key_cap()),
        Span::raw(" "),
        Span::styled(command.label, theme::key_label()),
        Span::raw(format!(" — {}", command.description)),
    ])
}

struct HelpCommand {
    key: &'static str,
    label: &'static str,
    description: &'static str,
}

fn help_commands(context: View, app: &SkilledApp) -> Vec<HelpCommand> {
    match context {
        View::Setup(step) => {
            let mut commands = Vec::new();
            if step == SetupStep::DetectAgents {
                commands.extend([
                    HelpCommand {
                        key: "Up/Down or j/k",
                        label: "Move",
                        description: "move the focused agent",
                    },
                    HelpCommand {
                        key: "Space",
                        label: "Toggle",
                        description: "toggle the focused agent",
                    },
                ]);
            }
            if step == SetupStep::DiscoverSources {
                commands.push(HelpCommand {
                    key: "a",
                    label: "Add source",
                    description: "inspect a local Git checkout",
                });
            }
            commands.push(if step == SetupStep::Summary {
                HelpCommand {
                    key: "Enter",
                    label: "Inventory",
                    description: "enter the Inventory view",
                }
            } else {
                HelpCommand {
                    key: "Enter",
                    label: "Continue",
                    description: "advance the setup flow",
                }
            });
            if step != SetupStep::Welcome {
                commands.push(HelpCommand {
                    key: "Esc",
                    label: "Back",
                    description: "return to the previous setup step",
                });
            }
            commands.extend([
                HelpCommand {
                    key: "?",
                    label: "Help",
                    description: "open this keyboard reference",
                },
                HelpCommand {
                    key: "q",
                    label: "Quit",
                    description: "quit when no dialog is open",
                },
            ]);
            commands
        }
        View::Inventory => vec![
            HelpCommand {
                key: "2",
                label: "Sources",
                description: "open registered sources",
            },
            HelpCommand {
                key: "s",
                label: "Settings",
                description: "open global settings",
            },
            HelpCommand {
                key: "?",
                label: "Help",
                description: "open this keyboard reference",
            },
            HelpCommand {
                key: "q",
                label: "Quit",
                description: "quit when no dialog is open",
            },
        ],
        View::Sources => {
            let mut commands = vec![
                HelpCommand {
                    key: "Tab / Shift-Tab",
                    label: "Region",
                    description: "move region focus forward or backward",
                },
                HelpCommand {
                    key: "Up/Down or j/k",
                    label: "Move",
                    description: "move repository or variant selection",
                },
            ];
            if app.sources_pane() != SourcesPane::Details {
                commands.push(HelpCommand {
                    key: "Enter",
                    label: "Open next region",
                    description: "advance toward Details",
                });
            }
            commands.extend([
                HelpCommand {
                    key: "a",
                    label: "Add source",
                    description: "inspect a local checkout",
                },
                HelpCommand {
                    key: "1",
                    label: "Inventory",
                    description: "return to Inventory",
                },
                HelpCommand {
                    key: "Esc",
                    label: "Back one region",
                    description: "return toward Repositories, then Inventory",
                },
                HelpCommand {
                    key: "?",
                    label: "Help",
                    description: "open this keyboard reference",
                },
                HelpCommand {
                    key: "q",
                    label: "Quit",
                    description: "quit when no dialog is open",
                },
            ]);
            commands
        }
        View::Settings => vec![
            HelpCommand {
                key: "Enter",
                label: "Rerun setup",
                description: "reset setup and start again",
            },
            HelpCommand {
                key: "Esc",
                label: "Close Settings",
                description: "return to Inventory after help closes",
            },
            HelpCommand {
                key: "?",
                label: "Help",
                description: "open this keyboard reference",
            },
        ],
    }
}

fn help_scope(context: View) -> String {
    match context {
        View::Setup(step) => format!("Setup · {}", step.title()),
        View::Inventory => "Inventory".to_owned(),
        View::Sources => "Sources".to_owned(),
        View::Settings => "Settings".to_owned(),
    }
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp) {
    frame.render_widget(
        Paragraph::new(components::key_hint_line(&key_hints(app), area.width))
            .style(theme::chrome()),
        area,
    );
}

/// The commands the active context actually handles.
///
/// This mirrors [`crate::input`]. A hint that is not backed by a key mapping is
/// a promise the application cannot keep, so unimplemented commands —
/// installation, updates, repair, uninstall, forget, and filtering — are absent
/// by construction.
fn key_hints(app: &SkilledApp) -> Vec<KeyHint> {
    if app.help_context().is_some() {
        return vec![
            KeyHint::essential("Esc", "Close"),
            KeyHint::new("Ctrl-C", "Quit"),
        ];
    }
    if app.source_path_input_active() {
        return vec![
            KeyHint::essential("Enter", "Inspect"),
            KeyHint::essential("Esc", "Cancel"),
            KeyHint::new("Ctrl-C", "Quit"),
        ];
    }
    if app.pending_source().is_some() {
        return vec![
            KeyHint::new("j/k", "Move"),
            KeyHint::new("Space", "Include"),
            KeyHint::new("c", "Class"),
            KeyHint::new("1/2/3", "Agents"),
            KeyHint::essential("Enter", "Register"),
            KeyHint::essential("Esc", "Cancel"),
        ];
    }
    match app.view() {
        View::Setup(step) => {
            let mut hints = Vec::new();
            if step == SetupStep::DetectAgents {
                hints.push(KeyHint::new("j/k", "Move"));
                hints.push(KeyHint::new("Space", "Toggle"));
            }
            if step == SetupStep::DiscoverSources {
                hints.push(KeyHint::new("a", "Add source"));
            }
            hints.push(KeyHint::essential(
                "Enter",
                if step == SetupStep::Summary {
                    "Inventory"
                } else {
                    "Continue"
                },
            ));
            // Step one has nowhere to go back to.
            if step != SetupStep::Welcome {
                hints.push(KeyHint::essential("Esc", "Back"));
            }
            hints.push(KeyHint::new("?", "Help"));
            hints.push(KeyHint::new("q", "Quit"));
            hints
        }
        View::Inventory => vec![
            KeyHint::new("2", "Sources"),
            KeyHint::new("s", "Settings"),
            KeyHint::new("?", "Help"),
            KeyHint::new("q", "Quit"),
        ],
        View::Sources => {
            let mut hints = vec![KeyHint::new("Tab/Shift-Tab", "Region")];
            if app.sources_pane() != SourcesPane::Details {
                hints.push(KeyHint::new("j/k", "Move"));
                hints.push(KeyHint::essential("Enter", "Open"));
            }
            hints.extend([
                KeyHint::new("a", "Add source"),
                KeyHint::new("1", "Inventory"),
                KeyHint::new("?", "Help"),
                KeyHint::new("q", "Quit"),
                KeyHint::essential("Esc", "Back"),
            ]);
            hints
        }
        View::Settings => vec![
            KeyHint::essential("Enter", "Rerun setup"),
            KeyHint::new("?", "Help"),
            KeyHint::essential("Esc", "Close"),
        ],
    }
}

fn render_size_notice(frame: &mut Frame<'_>, area: Rect) {
    frame.render_widget(Block::new().style(theme::app_surface()), area);
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
