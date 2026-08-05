use std::path::Path;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Margin, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap},
};

use crate::{
    AgentKind, InventoryPane, SetupStep, SkilledApp, SourcesPane, View,
    components::{self, KeyHint},
    inventory::{
        Finding, FindingSeverity, InstallationHealth, InstallationObject,
        InstalledSkillObservation, InventoryRow, RootScan, RootStatus,
    },
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
        View::Inventory => render_inventory(frame, body, app),
        View::Sources => render_sources(frame, body, app),
        View::Settings => {
            render_inventory(frame, body, app);
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
                Span::styled(format!("  {note}"), theme::nav_note()),
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
    // The filter is a text field rather than a dialog, but it still takes every
    // printable key, so the destination digits would not work while it is open.
    if app.inventory_filter_active() {
        return Some((
            "Filter inventory".to_owned(),
            "navigation is locked while the filter is open",
        ));
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
    if step == SetupStep::ConfirmCatalogs && app.pending_source().is_some() {
        render_registration_footer(frame, regions);
    } else {
        frame.render_widget(
            Paragraph::new(components::rule(regions.divider.width)),
            regions.divider,
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "Setup is persisted when it finishes",
                theme::key_label(),
            ))),
            regions.status,
        );
        frame.render_widget(
            Paragraph::new(setup_action_line(step).right_aligned()),
            regions.actions,
        );
    }
}

fn setup_action_line(step: SetupStep) -> Line<'static> {
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
        SetupStep::ScanInstallations => {
            lines.push(Line::from(
                "Skilled read the global skill root of each selected agent.",
            ));
            lines.push(Line::default());
            for root in app.inventory().roots() {
                lines.push(Line::from(vec![
                    components::badge(root_tone(root), &root.status().summary()),
                    Span::raw(format!(
                        "   {:<11}  {}",
                        root.agent().display_name(),
                        terminal_safe(&home_relative(root.path(), app.home()))
                    )),
                ]));
            }
            lines.extend([
                Line::default(),
                Line::from("Nothing outside those roots was read, and nothing was changed."),
            ]);
        }
        SetupStep::Summary => {
            let inventory = app.inventory();
            lines.extend([
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
                Line::from(format!(
                    "Installed: {}   Unmanaged: {}   Findings: {}",
                    inventory.installation_count(),
                    inventory.unmanaged_count(),
                    inventory.finding_count()
                )),
                Line::default(),
                Line::from("Unresolved findings never force a repair."),
            ]);
        }
    }
    lines
}

/// The Inventory workspace: one row per installed skill, and its detail.
///
/// A wide terminal shows the table and the detail region together; a compact
/// one shows whichever region has focus, so `Enter` is a drill-in and `Esc`
/// comes back.
fn render_inventory(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp) {
    match viewport::workspace_regions(area) {
        (primary, Some(detail)) => {
            render_inventory_skills(frame, primary, app);
            render_inventory_detail(frame, detail, app, true);
        }
        (primary, None) => match app.inventory_pane() {
            InventoryPane::Skills => render_inventory_skills(frame, primary, app),
            InventoryPane::Details => render_inventory_detail(frame, primary, app, false),
        },
    }
}

/// Column widths for the installation table.
///
/// The three agent columns and the health column are sized by their headings,
/// which never change; the identity columns divide whatever is left, so the
/// table keeps its shape from a sixty-column detail split up to any width.
#[derive(Clone, Copy)]
struct InventoryColumns {
    skill: usize,
    source: usize,
}

const AGENT_COLUMN_WIDTHS: [usize; 3] = [8, 7, 10];
const HEALTH_COLUMN_WIDTH: usize = 11;
/// The marker and its trailing space, contributed by `components::list_row`.
const ROW_MARKER_WIDTH: usize = 2;

fn inventory_columns(width: u16) -> InventoryColumns {
    let fixed = ROW_MARKER_WIDTH + AGENT_COLUMN_WIDTHS.iter().sum::<usize>() + HEALTH_COLUMN_WIDTH;
    let remaining = usize::from(width).saturating_sub(fixed);
    let skill = (remaining * 6 / 10).max(6);
    InventoryColumns {
        skill,
        source: remaining.saturating_sub(skill).max(4),
    }
}

fn render_inventory_skills(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp) {
    let rows = app.filtered_rows();
    let roots = inventory_roots_line(app);
    let roots_height = wrapped_line_count(&roots, area.width).min(2);
    let filter_height =
        u16::from(app.inventory_filter_active() || !app.inventory_filter().is_empty());
    let [header, body] = Layout::vertical([
        Constraint::Length(2 + u16::try_from(roots_height).unwrap_or(1) + filter_height),
        Constraint::Min(1),
    ])
    .areas(area);

    let mut header_lines = vec![components::focused_pane_header(
        "Global inventory",
        &inventory_subtitle(app, rows.len()),
        app.inventory_pane() == InventoryPane::Skills,
    )];
    if filter_height == 1 {
        header_lines.push(inventory_filter_line(app));
    }
    header_lines.push(roots);
    header_lines.push(components::rule(header.width));
    frame.render_widget(
        Paragraph::new(header_lines).wrap(Wrap { trim: false }),
        header,
    );

    if rows.is_empty() {
        let region = body.inner(Margin {
            horizontal: 2,
            vertical: 0,
        });
        let (headline, explanation) = inventory_empty_state(app);
        frame.render_widget(
            components::empty_state("⌕", &headline, &explanation, region),
            region,
        );
        return;
    }

    let columns = inventory_columns(body.width);
    let mut lines = vec![inventory_column_headings(columns)];
    let capacity = usize::from(body.height.max(1)).saturating_sub(1);
    let start = visible_window_start(app.focused_installation(), capacity);
    lines.extend(
        rows.iter()
            .enumerate()
            .skip(start)
            .take(capacity)
            .map(|(index, row)| {
                inventory_row_line(
                    row,
                    columns,
                    index == app.focused_installation(),
                    body.width,
                )
            }),
    );
    frame.render_widget(Paragraph::new(lines), body);
}

fn inventory_subtitle(app: &SkilledApp, shown: usize) -> String {
    let total = app.inventory().rows().len();
    if !app.inventory_filter().trim().is_empty() {
        return format!("{shown} of {total} skills");
    }
    match total {
        0 => "nothing installed".to_owned(),
        1 => "1 skill".to_owned(),
        total => format!("{total} skills"),
    }
}

/// The query box, or the query that is still narrowing the list.
fn inventory_filter_line(app: &SkilledApp) -> Line<'static> {
    let query = terminal_safe(app.inventory_filter());
    if app.inventory_filter_active() {
        return Line::from(vec![
            Span::styled("/", theme::section_title()),
            Span::raw(query),
            Span::styled(components::FOCUS_MARKER, theme::focus_marker()),
        ]);
    }
    Line::from(vec![
        Span::styled("Filter: ", theme::pane_subtitle()),
        Span::raw(query),
    ])
}

/// What each agent's root contributed, so a `-` cell can be read correctly.
fn inventory_roots_line(app: &SkilledApp) -> Line<'static> {
    let mut spans = vec![Span::styled("Roots: ", theme::pane_subtitle())];
    for (position, root) in app.inventory().roots().iter().enumerate() {
        if position > 0 {
            spans.push(Span::styled(" · ", theme::pane_subtitle()));
        }
        spans.push(Span::raw(format!("{} ", root.agent().display_name())));
        spans.push(Span::styled(
            root.status().short_summary(),
            theme::tone_style(root_tone(root)),
        ));
    }
    Line::from(spans)
}

fn root_tone(root: &RootScan) -> Tone {
    match root.status() {
        RootStatus::Scanned { .. } => Tone::Healthy,
        RootStatus::NotSelected | RootStatus::Missing => Tone::Inactive,
        RootStatus::Unreadable { .. } => Tone::Critical,
    }
}

fn inventory_column_headings(columns: InventoryColumns) -> Line<'static> {
    let mut heading = " ".repeat(ROW_MARKER_WIDTH);
    heading.push_str(&padded("Skill", columns.skill));
    heading.push_str(&padded("Source", columns.source));
    for (label, width) in ["Claude", "Codex", "OpenCode"]
        .into_iter()
        .zip(AGENT_COLUMN_WIDTHS)
    {
        heading.push_str(&padded(label, width));
    }
    heading.push_str("Health");
    Line::from(Span::styled(heading, theme::pane_subtitle()))
}

fn inventory_row_line(
    row: &InventoryRow,
    columns: InventoryColumns,
    selected: bool,
    width: u16,
) -> Line<'static> {
    let mut spans = vec![
        Span::raw(padded(&terminal_safe(row.name()), columns.skill)),
        Span::raw(padded(
            &terminal_safe(row.source_label().unwrap_or("unmanaged")),
            columns.source,
        )),
    ];
    for (agent, width) in AgentKind::ALL.into_iter().zip(AGENT_COLUMN_WIDTHS) {
        let tone = row
            .observation(agent)
            .map_or(Tone::Inactive, |observation| {
                installation_tone(observation.health())
            });
        spans.push(Span::styled(
            padded(components::tone_glyph(tone), width),
            theme::tone_style(tone),
        ));
    }
    let tone = installation_tone(row.health());
    spans.push(components::badge(tone, row.health().label()));
    components::list_row(spans, selected, width)
}

fn installation_tone(health: InstallationHealth) -> Tone {
    match health {
        InstallationHealth::Healthy => Tone::Healthy,
        InstallationHealth::Unmanaged => Tone::Unmanaged,
        InstallationHealth::Broken => Tone::Critical,
    }
}

/// What an empty table can honestly say, given what the scan observed.
fn inventory_empty_state(app: &SkilledApp) -> (String, String) {
    if !app.inventory_filter().trim().is_empty() {
        return (
            "No skills match the filter".to_owned(),
            "Press Esc to clear the filter and show every installed skill again.".to_owned(),
        );
    }
    let roots = app.inventory().roots();
    if roots
        .iter()
        .any(|root| matches!(root.status(), RootStatus::Unreadable { .. }))
    {
        return (
            "An agent skill root could not be read".to_owned(),
            "Skilled reports nothing from a root it could not read in full \
             rather than reporting part of it. The Roots line above names the \
             root and what happened."
                .to_owned(),
        );
    }
    if roots
        .iter()
        .any(|root| matches!(root.status(), RootStatus::Scanned { .. }))
    {
        return (
            "No skills are installed".to_owned(),
            "The agent skill roots Skilled read hold no skill directories. \
             Nothing was created or changed."
                .to_owned(),
        );
    }
    (
        "No agent skill root exists yet".to_owned(),
        "Skilled looked for the documented global skill root of each selected \
         agent and found none of them. It did not create one."
            .to_owned(),
    )
}

/// The detail region: everything observed about the selected installation.
fn render_inventory_detail(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &SkilledApp,
    beside_the_table: bool,
) {
    let region = if beside_the_table {
        let [separator, region] =
            Layout::horizontal([Constraint::Length(1), Constraint::Min(1)]).areas(area);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled("│", theme::rule()));
                usize::from(separator.height)
            ]),
            separator,
        );
        region.inner(Margin {
            horizontal: 1,
            vertical: 0,
        })
    } else {
        area.inner(Margin {
            horizontal: 1,
            vertical: 0,
        })
    };

    let selected = app.selected_installation();
    let [header, body] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(region);
    frame.render_widget(
        Paragraph::new(vec![
            components::focused_pane_header(
                "Details",
                &selected.map_or_else(
                    || "no selection".to_owned(),
                    |row| terminal_safe(row.name()),
                ),
                app.inventory_pane() == InventoryPane::Details,
            ),
            components::rule(header.width),
        ]),
        header,
    );

    let Some(row) = selected else {
        frame.render_widget(
            components::empty_state(
                "·",
                "Nothing to show",
                "Identity, provenance, and the observed object in every agent \
                 root appear here once a skill is selected.",
                body,
            ),
            body,
        );
        return;
    };
    frame.render_widget(
        Paragraph::new(inventory_detail_lines(row, app.home(), body.width))
            .wrap(Wrap { trim: false }),
        body,
    );
}

fn inventory_detail_lines(row: &InventoryRow, home: &Path, width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    push_detail_section(&mut lines, "SKILL", width);
    lines.push(detail_field("Name", row.name()));
    lines.push(Line::from(vec![
        Span::styled("Health: ", theme::pane_subtitle()),
        components::badge(installation_tone(row.health()), row.health().label()),
    ]));
    if let Some(SkillValidation::Valid { description, .. }) = row
        .observations()
        .find_map(InstalledSkillObservation::validation)
    {
        lines.push(detail_field_bounded("Description", description, width, 3));
    }

    push_detail_section(&mut lines, "SOURCE", width);
    match row
        .observations()
        .find_map(InstalledSkillObservation::resolution)
    {
        Some(resolution) => {
            lines.push(detail_field("Source", resolution.source_label()));
            lines.push(detail_field_bounded(
                "Catalog",
                &resolution.catalog_relative_path().display().to_string(),
                width,
                2,
            ));
            lines.push(detail_field_bounded(
                "Variant",
                &resolution.variant_relative_path().display().to_string(),
                width,
                2,
            ));
        }
        // Unresolved content is observed, never adopted.
        None => lines.push(Line::from(
            "Not resolved to any registered source; Skilled does not manage it.",
        )),
    }

    for agent in AgentKind::ALL {
        push_detail_section(&mut lines, &agent.display_name().to_uppercase(), width);
        match row.observation(agent) {
            Some(observation) => lines.extend(observation_lines(observation, home, width)),
            None => lines.push(Line::from(components::badge(
                Tone::Inactive,
                "not installed in this root",
            ))),
        }
    }
    lines
}

fn observation_lines(
    observation: &InstalledSkillObservation,
    home: &Path,
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        detail_field_bounded("Path", &home_relative(observation.path(), home), width, 2),
        detail_field("Object", observation.object().description()),
    ];
    if let InstallationObject::Symlink { target } = observation.object() {
        lines.push(detail_field_bounded(
            "Target",
            &home_relative(target, home),
            width,
            2,
        ));
    }
    lines.push(match observation.validation() {
        Some(SkillValidation::Valid { name, .. }) => Line::from(vec![
            Span::styled("Validation: ", theme::pane_subtitle()),
            components::badge(Tone::Healthy, "valid"),
            Span::raw(format!(" as {}", terminal_safe(name))),
        ]),
        Some(SkillValidation::Invalid { .. }) => Line::from(vec![
            Span::styled("Validation: ", theme::pane_subtitle()),
            components::badge(Tone::Critical, "invalid"),
        ]),
        None => Line::from(vec![
            Span::styled("Validation: ", theme::pane_subtitle()),
            components::badge(Tone::Inactive, "not attempted"),
        ]),
    });
    lines.extend(
        observation
            .findings()
            .iter()
            .flat_map(|finding| finding_lines(finding, width)),
    );
    lines
}

/// One finding: its stable code and severity, then the observation behind it.
///
/// The severity is spelled out beside the code, so the tone reinforces a word
/// the reader already has rather than carrying the meaning alone.
fn finding_lines(finding: &Finding, width: u16) -> [Line<'static>; 2] {
    let tone = match finding.severity() {
        FindingSeverity::Info => Tone::Inactive,
        FindingSeverity::Warning => Tone::Warning,
        FindingSeverity::Critical => Tone::Critical,
    };
    [
        Line::from(vec![
            Span::styled("Finding: ", theme::pane_subtitle()),
            Span::styled(
                format!("{} · {}", finding.code(), finding.severity().label()),
                theme::tone_style(tone),
            ),
        ]),
        Line::from(Span::raw(format!(
            "  {}",
            terminal_safe_bounded_start(
                finding.evidence(),
                usize::from(width).saturating_mul(3).saturating_sub(2)
            )
        ))),
    ]
}

/// An installation path as the user speaks about it.
///
/// Global roots are documented relative to the home directory, and a detail
/// region is too narrow to spend three wrapped lines on a prefix the reader
/// already knows. Anything outside the home directory stays absolute.
fn home_relative(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(relative) => format!("~/{}", relative.display()),
        Err(_) => path.display().to_string(),
    }
}

/// Bound a value to `width` cells and pad it out to exactly that.
fn padded(value: &str, width: usize) -> String {
    let bounded = terminal_safe_bounded_start(value, width.saturating_sub(1));
    let used = Span::raw(&bounded).width();
    format!("{bounded}{}", " ".repeat(width.saturating_sub(used)))
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
    let catalog_error_count = app
        .selected_source()
        .into_iter()
        .flat_map(RegisteredSource::catalogs)
        .filter(|catalog| catalog.scan_error().is_some())
        .count();
    let subtitle = match app.selected_source() {
        Some(source) if source.source_error().is_some() => "unavailable".to_owned(),
        Some(_) if catalog_error_count > 0 && variants.is_empty() => "scan unavailable".to_owned(),
        Some(_) if catalog_error_count > 0 => format!("{} found · scan error", variants.len()),
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

    let bounded_lines = 2;
    let mut repository_lines = Vec::new();
    push_detail_section(&mut repository_lines, "REPOSITORY", inner.width);
    repository_lines.push(detail_field_bounded(
        "Label",
        &format!(
            "{} · Branch: {}",
            terminal_safe(source.label()),
            terminal_safe(source.branch().unwrap_or("detached"))
        ),
        inner.width,
        1,
    ));
    repository_lines.push(detail_field_bounded(
        "Path",
        &source.git_top_level().display().to_string(),
        inner.width,
        bounded_lines,
    ));
    repository_lines.push(detail_field("HEAD", source.head()));
    repository_lines.push(detail_field_bounded(
        "Remote",
        source.remote_url().unwrap_or("not configured"),
        inner.width,
        bounded_lines,
    ));
    repository_lines.push(Line::from(vec![
        Span::styled("Status: ", theme::pane_subtitle()),
        source_status_badge(source),
        Span::raw(" · "),
        Span::styled("Last scan: ", theme::pane_subtitle()),
        Span::raw(source.last_scan_at().to_string()),
    ]));
    if let Some(error) = source.source_error() {
        repository_lines.push(detail_field_bounded("Source error", error, inner.width, 3));
    }

    let mut catalog_lines = Vec::new();
    push_detail_section(&mut catalog_lines, "CATALOG", inner.width);
    if let Some(variant) = selected {
        catalog_lines.push(detail_field_bounded(
            "Path",
            &format!(
                "{} · Classification: {}",
                terminal_safe(&variant.catalog.relative_path().display().to_string()),
                catalog_classification(variant.catalog)
            ),
            inner.width,
            if inner.width >= 60 { 1 } else { 2 },
        ));
        let compatibility = variant.catalog.compatibility();
        catalog_lines.push(detail_field_bounded(
            "Compatibility",
            &format!(
                "Claude Code: {} · Codex: {} · OpenCode: {}",
                yes_no(compatibility.claude_code()),
                yes_no(compatibility.codex()),
                yes_no(compatibility.opencode())
            ),
            inner.width,
            2,
        ));
    } else {
        catalog_lines.push(Line::from(
            "No variant selected; catalog metadata is unavailable.",
        ));
    }
    let catalog_essential_height = detail_lines_height(&catalog_lines, inner.width);
    for catalog in source
        .catalogs()
        .iter()
        .filter(|catalog| catalog.scan_error().is_some())
    {
        let error = catalog.scan_error().expect("filtered catalog error");
        catalog_lines.push(detail_field_bounded(
            "Catalog error",
            &format!(
                "{}: {}",
                terminal_safe(&catalog.relative_path().display().to_string()),
                terminal_safe(error)
            ),
            inner.width,
            3,
        ));
    }

    let mut variant_lines = Vec::new();
    push_detail_section(&mut variant_lines, "VARIANT", inner.width);
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
        variant_lines.push(detail_field_bounded(
            "Directory",
            &format!(
                "{} · Name: {}",
                terminal_safe(variant.candidate.directory_name()),
                terminal_safe(name)
            ),
            inner.width,
            1,
        ));
        variant_lines.push(detail_field_bounded(
            "Path",
            &variant.candidate.relative_path().display().to_string(),
            inner.width,
            1,
        ));
        variant_lines.push(Line::from(vec![
            Span::styled("Status: ", theme::pane_subtitle()),
            status,
        ]));
        let variant_essential_height = detail_lines_height(&variant_lines, inner.width);
        match variant.candidate.validation() {
            SkillValidation::Valid { description, .. } => {
                variant_lines.push(detail_field("Description", description));
            }
            SkillValidation::Invalid { message } => {
                variant_lines.push(detail_field("Validation error", message));
            }
        }
        render_detail_regions(
            frame,
            inner,
            repository_lines,
            catalog_lines,
            catalog_essential_height,
            variant_lines,
            variant_essential_height,
        );
    } else {
        variant_lines.push(Line::from("No variant selected."));
        let variant_essential_height = detail_lines_height(&variant_lines, inner.width);
        render_detail_regions(
            frame,
            inner,
            repository_lines,
            catalog_lines,
            catalog_essential_height,
            variant_lines,
            variant_essential_height,
        );
    }
}

fn render_detail_regions(
    frame: &mut Frame<'_>,
    area: Rect,
    repository_lines: Vec<Line<'static>>,
    catalog_lines: Vec<Line<'static>>,
    catalog_essential_height: usize,
    variant_lines: Vec<Line<'static>>,
    variant_essential_height: usize,
) {
    let available = usize::from(area.height);
    let reserved_variant = variant_essential_height.min(available);
    let reserved_catalog = catalog_essential_height.min(available.saturating_sub(reserved_variant));
    let repository_height = detail_lines_height(&repository_lines, area.width).min(
        available
            .saturating_sub(reserved_catalog)
            .saturating_sub(reserved_variant),
    );
    let after_repository = available.saturating_sub(repository_height);
    let catalog_height = detail_lines_height(&catalog_lines, area.width)
        .min(after_repository.saturating_sub(reserved_variant));
    let variant_height = after_repository.saturating_sub(catalog_height);
    let [repository_area, catalog_area, variant_area] = Layout::vertical([
        Constraint::Length(u16::try_from(repository_height).unwrap_or(u16::MAX)),
        Constraint::Length(u16::try_from(catalog_height).unwrap_or(u16::MAX)),
        Constraint::Length(u16::try_from(variant_height).unwrap_or(u16::MAX)),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(repository_lines).wrap(Wrap { trim: false }),
        repository_area,
    );
    frame.render_widget(
        Paragraph::new(catalog_lines).wrap(Wrap { trim: false }),
        catalog_area,
    );
    frame.render_widget(
        Paragraph::new(variant_lines).wrap(Wrap { trim: false }),
        variant_area,
    );
}

fn detail_lines_height(lines: &[Line<'_>], width: u16) -> usize {
    lines
        .iter()
        .map(|line| wrapped_line_count(line, width))
        .sum()
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
    let width = match viewport::classify(area) {
        viewport::Viewport::Compact => 76,
        viewport::Viewport::Wide => 80,
    };
    let popup = centered_rect(width, 12, area);
    frame.render_widget(Clear, popup);
    let block = components::dialog_frame("Add source", "local Git checkout");
    let regions = components::dialog_regions(block.inner(popup), 28);
    frame.render_widget(block, popup);
    let path = terminal_safe(app.source_path());
    if let Some(error) = app.source_error() {
        let error = vec![Line::from(components::badge(
            Tone::Critical,
            &terminal_safe(error),
        ))];
        let error_height = Paragraph::new(error.clone())
            .wrap(Wrap { trim: false })
            .line_count(regions.body.width)
            .min(usize::from(regions.body.height.saturating_sub(2)));
        let [input, error_area] = Layout::vertical([
            Constraint::Min(2),
            Constraint::Length(u16::try_from(error_height).unwrap_or(u16::MAX)),
        ])
        .areas(regions.body);
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("Local Git repository", theme::section_title()),
                Line::from(format!("> {path}")),
            ])
            .wrap(Wrap { trim: false }),
            input,
        );
        frame.render_widget(Paragraph::new(error).wrap(Wrap { trim: false }), error_area);
    } else {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("Local Git repository", theme::section_title()),
                Line::from("Enter a path inside a local Git checkout:"),
                Line::default(),
                Line::from(format!("> {path}")),
            ])
            .wrap(Wrap { trim: false }),
            regions.body,
        );
    }
    frame.render_widget(
        Paragraph::new(components::rule(regions.divider.width)),
        regions.divider,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Read-only checkout and catalog scan",
            theme::key_label(),
        ))),
        regions.status,
    );
    frame.render_widget(
        Paragraph::new(
            Line::from(vec![
                Span::styled("Esc", theme::key_cap()),
                Span::raw(" "),
                Span::styled("Cancel", theme::key_label()),
                Span::raw("   "),
                Span::styled("Enter", theme::key_cap()),
                Span::raw(" "),
                Span::styled("Inspect", theme::key_label()),
            ])
            .right_aligned(),
        ),
        regions.actions,
    );
}

fn render_catalog_confirmation(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp) {
    let (width, height) = match viewport::classify(area) {
        viewport::Viewport::Compact => (76, 18),
        viewport::Viewport::Wide => (104, 20),
    };
    let popup = centered_rect(width, height, area);
    frame.render_widget(Clear, popup);
    let block = components::dialog_frame("Confirm catalogs", "registration only");
    let regions = components::dialog_regions(block.inner(popup), 29);
    frame.render_widget(block, popup);

    render_catalog_confirmation_content(frame, regions.body, app);
    render_registration_footer(frame, regions);
}

fn render_catalog_confirmation_content(frame: &mut Frame<'_>, inner: Rect, app: &SkilledApp) {
    let (metadata, catalogs, error) = catalog_confirmation_sections(app, inner.width);
    let viewport_height = usize::from(inner.height);
    let focused_height = catalogs
        .get(app.focused_catalog())
        .map(|lines| confirmation_lines_height(lines, inner.width))
        .unwrap_or(0)
        .min(viewport_height);
    let error_height = confirmation_lines_height(&error, inner.width)
        .min(viewport_height.saturating_sub(focused_height));
    let metadata_height = confirmation_lines_height(&metadata, inner.width).min(
        viewport_height
            .saturating_sub(focused_height)
            .saturating_sub(error_height),
    );
    let catalog_height = viewport_height
        .saturating_sub(metadata_height)
        .saturating_sub(error_height);
    let visible_catalogs = visible_catalog_confirmation_lines(
        &catalogs,
        app.focused_catalog(),
        inner.width,
        catalog_height,
    );
    let visible_catalog_height =
        confirmation_lines_height(&visible_catalogs, inner.width).min(catalog_height);
    let mut spare_rows = viewport_height
        .saturating_sub(metadata_height)
        .saturating_sub(visible_catalog_height)
        .saturating_sub(error_height);
    let metadata_gap = !metadata.is_empty() && !visible_catalogs.is_empty() && spare_rows > 0;
    spare_rows = spare_rows.saturating_sub(usize::from(metadata_gap));
    let error_gap = !error.is_empty() && spare_rows > 0;

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
}

fn catalog_confirmation_sections(
    app: &SkilledApp,
    width: u16,
) -> (
    Vec<Line<'static>>,
    Vec<Vec<Line<'static>>>,
    Vec<Line<'static>>,
) {
    let mut metadata = Vec::new();
    let mut catalogs = Vec::new();
    if let Some(preview) = app.pending_source() {
        let source = preview.inspected();
        metadata.extend([
            confirmation_repository_line(source.git_top_level(), width),
            confirmation_branch_line(
                source.branch().unwrap_or("detached"),
                &source.head()[..source.head().len().min(8)],
                width,
            ),
            confirmation_field(
                "Remote",
                source.remote_url().unwrap_or("not configured"),
                width,
            ),
            Line::from(vec![
                Span::raw("Worktree: "),
                worktree_badge(source.dirty()),
            ]),
        ]);
        for (index, catalog) in preview.catalogs().iter().enumerate() {
            let count = catalog.candidates().len();
            let marker = if index == app.focused_catalog() {
                components::FOCUS_MARKER
            } else {
                " "
            };
            let inclusion = if catalog.included() {
                "Included"
            } else {
                "Excluded"
            };
            let prefix = format!(" {inclusion} · ");
            let suffix = format!(" · {count} candidate{}", if count == 1 { "" } else { "s" });
            let path_budget = usize::from(width).saturating_sub(
                Span::raw(marker).width() + Span::raw(&prefix).width() + Span::raw(&suffix).width(),
            );
            catalogs.push(vec![
                Line::from(vec![
                    Span::styled(marker, theme::focus_marker()),
                    Span::raw(prefix),
                    Span::raw(terminal_safe_bounded_middle(
                        &catalog.relative_path().display().to_string(),
                        path_budget,
                    )),
                    Span::raw(suffix),
                ]),
                Line::from(format!(
                    "  {} catalog · Claude Code: {} · Codex: {} · OpenCode: {}",
                    match catalog.classification() {
                        CatalogClassification::Common => "Common",
                        CatalogClassification::AgentSpecific => "Agent-specific",
                    },
                    yes_no(catalog.compatibility().claude_code()),
                    yes_no(catalog.compatibility().codex()),
                    yes_no(catalog.compatibility().opencode())
                )),
            ]);
        }
    }
    let mut error = Vec::new();
    if let Some(message) = app.source_error() {
        let budget = usize::from(width).saturating_mul(2).saturating_sub(2);
        error.push(Line::from(components::badge(
            Tone::Critical,
            &terminal_safe_bounded_middle(message, budget),
        )));
    }
    (metadata, catalogs, error)
}

fn confirmation_field(label: &str, value: &str, width: u16) -> Line<'static> {
    let prefix = format!("{label}: ");
    let value = terminal_safe_bounded_middle(
        value,
        usize::from(width).saturating_sub(Span::raw(&prefix).width()),
    );
    Line::from(vec![Span::raw(prefix), Span::raw(value)])
}

fn confirmation_repository_line(path: &std::path::Path, width: u16) -> Line<'static> {
    let value = terminal_safe(&path.display().to_string());
    let prefix = "Repository: ";
    let available = usize::from(width).saturating_sub(Span::raw(prefix).width());
    if Span::raw(&value).width() <= available {
        return Line::from(format!("{prefix}{value}"));
    }

    let label = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| value.clone());
    let label_budget = available.min(24);
    let label = terminal_safe_bounded_start(&label, label_budget);
    let separator = " · ";
    let path_budget = available
        .saturating_sub(Span::raw(&label).width())
        .saturating_sub(Span::raw(separator).width());
    let path = terminal_safe_bounded_middle(&value, path_budget);
    Line::from(format!("{prefix}{label}{separator}{path}"))
}

fn confirmation_branch_line(branch: &str, head: &str, width: u16) -> Line<'static> {
    let prefix = "Branch: ";
    let suffix = format!("   HEAD: {head}");
    let branch = terminal_safe_bounded_middle(
        branch,
        usize::from(width)
            .saturating_sub(Span::raw(prefix).width())
            .saturating_sub(Span::raw(&suffix).width()),
    );
    Line::from(vec![
        Span::raw(prefix),
        Span::raw(branch),
        Span::raw(suffix),
    ])
}

fn terminal_safe_bounded_middle(value: &str, budget: usize) -> String {
    const ELLIPSIS: &str = "...";
    let safe = terminal_safe(value);
    if Span::raw(&safe).width() <= budget {
        return safe;
    }
    if budget <= ELLIPSIS.len() {
        return ELLIPSIS[..budget].to_owned();
    }

    let content_budget = budget - ELLIPSIS.len();
    let prefix = display_prefix(&safe, content_budget.div_ceil(2));
    let suffix = display_suffix(&safe, content_budget / 2);
    format!("{prefix}{ELLIPSIS}{suffix}")
}

fn terminal_safe_bounded_start(value: &str, budget: usize) -> String {
    const ELLIPSIS: &str = "...";
    let safe = terminal_safe(value);
    if Span::raw(&safe).width() <= budget {
        return safe;
    }
    if budget <= ELLIPSIS.len() {
        return ELLIPSIS[..budget].to_owned();
    }
    format!(
        "{}{ELLIPSIS}",
        display_prefix(&safe, budget - ELLIPSIS.len())
    )
}

fn display_prefix(value: &str, budget: usize) -> String {
    let mut prefix = String::new();
    for character in value.chars() {
        let mut candidate = prefix.clone();
        candidate.push(character);
        if Span::raw(&candidate).width() > budget {
            break;
        }
        prefix.push(character);
    }
    prefix
}

fn display_suffix(value: &str, budget: usize) -> String {
    let mut suffix = String::new();
    for character in value.chars().rev() {
        let mut candidate = String::with_capacity(suffix.len() + character.len_utf8());
        candidate.push(character);
        candidate.push_str(&suffix);
        if Span::raw(&candidate).width() > budget {
            break;
        }
        suffix = candidate;
    }
    suffix
}

fn confirmation_lines_height(lines: &[Line<'_>], width: u16) -> usize {
    lines
        .iter()
        .map(|line| wrapped_line_count(line, width))
        .sum()
}

fn visible_catalog_confirmation_lines(
    entries: &[Vec<Line<'static>>],
    focused: usize,
    width: u16,
    height: usize,
) -> Vec<Line<'static>> {
    let Some(focused_entry) = entries.get(focused) else {
        return Vec::new();
    };
    let mut visible = focused_entry.clone();
    let mut used = confirmation_lines_height(&visible, width);

    for entry in entries[..focused].iter().rev() {
        let entry_height = confirmation_lines_height(entry, width);
        if used.saturating_add(entry_height) > height {
            break;
        }
        visible.splice(0..0, entry.clone());
        used = used.saturating_add(entry_height);
    }
    for entry in &entries[focused.saturating_add(1)..] {
        let entry_height = confirmation_lines_height(entry, width);
        if used.saturating_add(entry_height) > height {
            break;
        }
        visible.extend(entry.clone());
        used = used.saturating_add(entry_height);
    }
    visible
}

fn render_registration_footer(frame: &mut Frame<'_>, regions: components::DialogRegions) {
    frame.render_widget(
        Paragraph::new(components::rule(regions.divider.width)),
        regions.divider,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Registration records metadata only",
            theme::key_label(),
        ))),
        regions.status,
    );
    frame.render_widget(
        Paragraph::new(
            Line::from(vec![
                Span::styled("Esc", theme::key_cap()),
                Span::raw(" "),
                Span::styled("Cancel", theme::key_label()),
                Span::raw("   "),
                Span::styled("Enter", theme::key_cap()),
                Span::raw(" "),
                Span::styled("Register", theme::key_label()),
            ])
            .right_aligned(),
        ),
        regions.actions,
    );
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
        View::Inventory => {
            let mut commands = vec![HelpCommand {
                key: "Tab / Shift-Tab",
                label: "Region",
                description: "move region focus forward or backward",
            }];
            if app.filtered_rows().len() > 1 {
                commands.push(HelpCommand {
                    key: "Up/Down or j/k",
                    label: "Move",
                    description: "move the selected skill",
                });
            }
            if inventory_can_advance(app) {
                commands.push(HelpCommand {
                    key: "Enter",
                    label: "Open details",
                    description: "show everything observed about the selection",
                });
            }
            if !app.inventory().rows().is_empty() {
                commands.push(HelpCommand {
                    key: "/",
                    label: "Filter",
                    description: "narrow by name, source, or health",
                });
            }
            commands.extend([
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
            ]);
            if inventory_can_go_back(app) {
                commands.push(HelpCommand {
                    key: "Esc",
                    label: "Back",
                    description: "clear the filter, then leave the detail region",
                });
            }
            commands
        }
        View::Sources => {
            let mut commands = vec![HelpCommand {
                key: "Tab / Shift-Tab",
                label: "Region",
                description: "move region focus forward or backward",
            }];
            if sources_can_move_selection(app) {
                commands.push(HelpCommand {
                    key: "Up/Down or j/k",
                    label: "Move",
                    description: "move repository or variant selection",
                });
            }
            if sources_can_advance(app) {
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
    if app.inventory_filter_active() {
        return vec![
            KeyHint::essential("Enter", "Apply"),
            KeyHint::essential("Esc", "Clear"),
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
        View::Inventory => {
            let mut hints = vec![KeyHint::new("Tab/Shift-Tab", "Region")];
            if app.filtered_rows().len() > 1 {
                hints.push(KeyHint::new("j/k", "Move"));
            }
            if inventory_can_advance(app) {
                hints.push(KeyHint::essential("Enter", "Open"));
            }
            if !app.inventory().rows().is_empty() {
                hints.push(KeyHint::new("/", "Filter"));
            }
            hints.extend([
                KeyHint::new("2", "Sources"),
                KeyHint::new("s", "Settings"),
                KeyHint::new("?", "Help"),
                KeyHint::new("q", "Quit"),
            ]);
            if inventory_can_go_back(app) {
                hints.push(KeyHint::essential("Esc", "Back"));
            }
            hints
        }
        View::Sources => {
            let mut hints = vec![KeyHint::new("Tab/Shift-Tab", "Region")];
            if sources_can_move_selection(app) {
                hints.push(KeyHint::new("j/k", "Move"));
            }
            if sources_can_advance(app) {
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

/// Enter only drills in, so it advertises itself only where it can.
fn inventory_can_advance(app: &SkilledApp) -> bool {
    app.inventory_pane() == InventoryPane::Skills && app.selected_installation().is_some()
}

/// Back unwinds an applied filter, then a drilled-in detail region.
fn inventory_can_go_back(app: &SkilledApp) -> bool {
    !app.inventory_filter().is_empty() || app.inventory_pane() == InventoryPane::Details
}

fn sources_can_move_selection(app: &SkilledApp) -> bool {
    match app.sources_pane() {
        SourcesPane::Repositories => app.sources().len() > 1,
        SourcesPane::Variants => app
            .selected_source()
            .is_some_and(|source| flattened_variants(source).len() > 1),
        SourcesPane::Details => false,
    }
}

fn sources_can_advance(app: &SkilledApp) -> bool {
    match app.sources_pane() {
        SourcesPane::Repositories | SourcesPane::Variants => app.selected_source().is_some(),
        SourcesPane::Details => false,
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
