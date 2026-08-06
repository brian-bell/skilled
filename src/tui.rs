use std::path::Path;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Margin, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap},
};

use crate::{
    AgentKind, InventoryPane, SetupStep, SkilledApp, SourcesPane, View,
    app::MAX_INVENTORY_FILTER,
    components::{self, KeyHint},
    inventory::{
        Finding, FindingSeverity, InstallationHealth, InstallationObject,
        InstalledSkillObservation, InventoryRow, RootScan, RootStatus, RowProvenance,
    },
    source::{
        CatalogClassification, CatalogProposal, Compatibility, RegisteredSource, SkillCandidate,
        SkillValidation,
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
    //
    // The band goes down first and the paragraphs only carry foreground
    // colours, so it survives underneath them.
    frame.render_widget(Block::new().style(theme::chrome_band()), area);

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
/// Skilled performs no network access in this release, so the status may only
/// describe setup progress and what the local scan observed.
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
        // The accent is patched over the entry's own style, so the count keeps
        // the surface of the tab it belongs to. A bare digit before a title
        // is a route key; a '·'-led digit after a title is a count. The
        // prototype separates the two classes by colour alone (.tab-key
        // faint, .tab-count amber — see spec/tui-prototype.html:133-134),
        // which a terminal may not rest on: with three sources the row reads
        // '... Sources 3  Updates (soon) ...' and the trailing '3' binds
        // left or right only by a grammar the reader has not been taught.
        // '·' makes the class textual at every width and in any palette.
        if let Some(count) = destination.count(app) {
            spans.push(Span::styled(
                format!("·{count} "),
                style.patch(theme::nav_count()),
            ));
        }
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

    /// What this destination can honestly say it holds, if anything.
    ///
    /// The registry is always fully known, so Sources always has a count, zero
    /// included. The inventory is an observation of the filesystem, and
    /// whether that observation may be stated as a number is decided by
    /// [`crate::inventory::InventorySnapshot::stated_skill_count`] — the same decision
    /// [`inventory_subtitle`] defers to, so the tab and the subtitle beneath it
    /// cannot disagree. A destination this release cannot open has nothing to
    /// count and renders nothing: an em dash would read as a measurement that
    /// came back empty.
    fn count(self, app: &SkilledApp) -> Option<usize> {
        match self {
            Self::Inventory => app.inventory().stated_skill_count(),
            Self::Sources => Some(app.sources().len()),
            Self::Updates | Self::Doctor => None,
        }
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
            // A root that could not be read was attempted, not read; the
            // sentence follows the statuses below it rather than asserting a
            // success the scan did not have.
            lines.push(Line::from(
                if app.inventory().unreadable_roots().next().is_some() {
                    "Skilled attempted to read the global skill root of each selected agent."
                } else {
                    "Skilled read the global skill root of each selected agent."
                },
            ));
            lines.push(Line::default());
            // The status badges vary in width, so they are padded to a column
            // and the agent names line up beneath one another.
            const STATUS_COLUMN: usize = 19;
            for root in app.inventory().roots() {
                let badge = components::badge(root_tone(root), &root.status().summary());
                let padding = STATUS_COLUMN.saturating_sub(badge.width());
                lines.push(Line::from(vec![
                    badge,
                    Span::raw(format!(
                        "{}{:<11}  {}",
                        " ".repeat(padding),
                        root.agent().display_name(),
                        terminal_safe(&home_relative(root.path(), app.home()))
                    )),
                ]));
                // A root that could not be read contributed nothing above, so
                // its reason is the only account of it — same as the Inventory
                // header. The message is bounded so an operating-system error
                // cannot displace the line that closes the step.
                if let RootStatus::Unreadable { message } = root.status() {
                    let badge = components::badge(Tone::Critical, root.agent().display_name());
                    let budget = usize::from(width)
                        .saturating_mul(2)
                        .saturating_sub(badge.width() + 2);
                    lines.push(Line::from(vec![
                        badge,
                        Span::raw(format!(
                            ": {}",
                            terminal_safe_bounded_start(message, budget)
                        )),
                    ]));
                }
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
                if inventory.stated_skill_count().is_none() {
                    // The same verdict the Inventory surfaces defer to: a total
                    // taken across roots that were not read would read as
                    // "none installed" when it means "not known", and a scan
                    // that only found roots absent earns a phrase, not a zero.
                    Line::from(components::badge(
                        Tone::Inactive,
                        if inventory.unreadable_roots().next().is_some() {
                            "installation counts unavailable: a skill root could not be read"
                        } else {
                            "installation counts unavailable: no skill root was read"
                        },
                    ))
                } else {
                    Line::from(format!(
                        "Installed: {}   Unmanaged: {}   Broken: {}",
                        inventory.installation_count(),
                        inventory.unmanaged_count(),
                        inventory.broken_count()
                    ))
                },
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
/// which never change; the identity columns divide whatever is left, up to a
/// cap.
#[derive(Clone, Copy)]
struct InventoryColumns {
    skill: usize,
    source: usize,
}

const AGENT_COLUMN_WIDTHS: [usize; 3] = [8, 7, 10];
/// Wide enough for the longest health badge — `- not a skill`, thirteen cells
/// — plus a column of clearance, so the row is never clipped and never abuts
/// the detail region's separator.
const HEALTH_COLUMN_WIDTH: usize = 14;
/// The marker and its trailing space, contributed by `components::list_row`.
const ROW_MARKER_WIDTH: usize = 2;
/// Below this, a Source column would only ever show an ellipsis.
const MINIMUM_SOURCE_WIDTH: usize = 12;
const MINIMUM_SKILL_WIDTH: usize = 8;
/// Past these, an identity column stops earning its width: a short label such
/// as `not registered` is left stranded in whitespace, and every row is pulled
/// so far apart that a name and its health verdict no longer read as one line.
/// The cap bounds that measure instead.
///
/// It is not free. A name or source label longer than the cap is ellipsized
/// here where a wider terminal could have shown it whole; the detail region
/// beside the table still gives both in full, so nothing is only knowable from
/// this column.
///
/// This is a deliberate departure from the prototype rather than a translation
/// of it: that grid gives the same columns floors that grow without bound
/// (`minmax(145px, 1.5fr)`, `minmax(110px, 1fr)`), and grows its Health column
/// too (`minmax(92px, .8fr)`), so it never leaves the slack this does.
const MAX_SKILL_WIDTH: usize = 36;
const MAX_SOURCE_WIDTH: usize = 24;

fn inventory_columns(width: u16) -> InventoryColumns {
    let fixed = ROW_MARKER_WIDTH + AGENT_COLUMN_WIDTHS.iter().sum::<usize>() + HEALTH_COLUMN_WIDTH;
    let remaining = usize::from(width).saturating_sub(fixed);
    let skill = (remaining * 6 / 10).clamp(MINIMUM_SKILL_WIDTH, MAX_SKILL_WIDTH);
    let source = remaining.saturating_sub(skill).min(MAX_SOURCE_WIDTH);
    // A Source column too narrow to hold a label truncates every source to the
    // same ellipsis, which distinguishes nothing. The whole column is dropped
    // instead, and the detail region still names the source.
    if source < MINIMUM_SOURCE_WIDTH {
        // The clamp mirrors the exit below so the two agree about the cap;
        // its upper bound cannot bind here, because this branch is only
        // reached when `remaining` is well under `MAX_SKILL_WIDTH`.
        return InventoryColumns {
            skill: remaining.clamp(MINIMUM_SKILL_WIDTH, MAX_SKILL_WIDTH),
            source: 0,
        };
    }
    InventoryColumns { skill, source }
}

fn render_inventory_skills(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp) {
    let rows = app.filtered_rows();
    let mut header_lines = vec![pane_header(
        "Global inventory",
        &inventory_subtitle(app, rows.len()),
        app.inventory_pane() == InventoryPane::Skills,
        area.width,
    )];
    if app.inventory_filter_active() || !app.inventory_filter().is_empty() {
        header_lines.push(inventory_filter_line(app));
    }
    header_lines.push(inventory_roots_line(app));
    // A root that could not be read contributes nothing, so its reason is the
    // only account the user gets of what is inside it. Every root that failed
    // gets its own line: dropping the second would hide a second obstruction.
    header_lines.extend(app.inventory().unreadable_roots().map(|(root, message)| {
        let badge = components::badge(Tone::Critical, root.agent().display_name());
        // The reason carries an operating-system message; two rows of it must
        // not squeeze out the table it sits above.
        let budget = usize::from(area.width)
            .saturating_mul(2)
            .saturating_sub(badge.width() + 2);
        Line::from(vec![
            badge,
            Span::raw(format!(
                ": {}",
                terminal_safe_bounded_start(message, budget)
            )),
        ])
    }));

    // Measured after the lines exist, so a wrapped root status or a second
    // failure reason cannot displace the rule that closes the header.
    let header_height = detail_lines_height(&header_lines, area.width)
        .saturating_add(1)
        .min(usize::from(area.height.saturating_sub(1)));
    let [header, body] = Layout::vertical([
        Constraint::Length(u16::try_from(header_height).unwrap_or(u16::MAX)),
        Constraint::Min(1),
    ])
    .areas(area);
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
    let inventory = app.inventory();
    let total = inventory.rows().len();
    // A snapshot nothing has been read into has no rows for a filter to
    // narrow: "not scanned" is the only backed claim, and outranks both the
    // filter's count and every hedge below. The filter itself is kept — it
    // narrows the scan that lands next. Deselected roots may sit beside
    // pending ones; scan_pending tolerates that mixture.
    if inventory.scan_pending() {
        return "not scanned".to_owned();
    }
    // All-deselected is also outside the filter's reach: nothing was ever in
    // scope to list, so "0 of 0 listed" would invent a completed scan of an
    // empty table rather than say no agent is configured.
    if inventory.no_agent_configured() {
        return "no root read".to_owned();
    }
    if !app.inventory_filter().trim().is_empty() {
        return format!("{shown} of {total} listed");
    }
    // A positive count is a claim about every selected root. When a root
    // could not be read, the rows that were read are only "listed": stating
    // "N skills" here would read as a complete total while part of the
    // requested scope contributed nothing.
    //
    // Rows only come from a root that was read, so a withheld count means an
    // incomplete scope here and never an unread one: this branch cannot be
    // reached by the "nothing was read at all" half of the rule below.
    if total > 0 && inventory.stated_skill_count().is_none() {
        return format!("{total} listed · not fully read");
    }
    // Skills and stray content are counted apart: a root holding only a
    // README must not be described as holding a skill.
    let skills = inventory.skill_row_count();
    let other = total - skills;
    if other > 0 {
        return format!(
            "{skills} skill{} · {other} other entr{}",
            if skills == 1 { "" } else { "s" },
            if other == 1 { "y" } else { "ies" }
        );
    }
    // Whether a number may be stated at all is decided once, in the snapshot.
    // What is left here is the wording: a stated count says how much was
    // found, and a withheld one says which of the three reasons it was
    // withheld for.
    match inventory.stated_skill_count() {
        Some(0) => "nothing installed".to_owned(),
        Some(1) => "1 skill".to_owned(),
        Some(skills) => format!("{skills} skills"),
        None if inventory.unreadable_roots().next().is_some() => "not fully read".to_owned(),
        None => "no root read".to_owned(),
    }
}

/// The query box, or the query that is still narrowing the list.
///
/// The query is bounded on entry, and bounded again here: the header must
/// never grow at the expense of the table the query exists to narrow.
fn inventory_filter_line(app: &SkilledApp) -> Line<'static> {
    let query = terminal_safe_bounded_start(app.inventory_filter(), MAX_INVENTORY_FILTER);
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

/// What each agent's root contributed.
///
/// A `-` cell means no skill is installed under that name in that root, which
/// covers both "nothing is there" and "something is there that is not a skill";
/// the Health column names which. This line supplies the other half a `-` needs
/// to be read: whether the root was scanned at all.
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
        RootStatus::NotScanned | RootStatus::NotSelected | RootStatus::Missing => Tone::Inactive,
        RootStatus::Unreadable { .. } => Tone::Critical,
    }
}

/// The table's column headings, uppercased as in the prototype's `.grid-head`.
///
/// The prototype sets that row in its faint grey; MUTED is the recorded
/// substitution, because a heading that names a column is information-bearing
/// and has to meet 4.5:1.
fn inventory_column_headings(columns: InventoryColumns) -> Line<'static> {
    let mut heading = " ".repeat(ROW_MARKER_WIDTH);
    heading.push_str(&padded("SKILL", columns.skill));
    heading.push_str(&padded("SOURCE", columns.source));
    for (label, width) in ["CLAUDE", "CODEX", "OPENCODE"]
        .into_iter()
        .zip(AGENT_COLUMN_WIDTHS)
    {
        heading.push_str(&padded(label, width));
    }
    heading.push_str("HEALTH");
    Line::from(Span::styled(heading, theme::pane_subtitle()))
}

fn inventory_row_line(
    row: &InventoryRow,
    columns: InventoryColumns,
    selected: bool,
    width: u16,
) -> Line<'static> {
    let provenance = row.provenance();
    let source = padded(&terminal_safe(provenance.label()), columns.source);
    let mut spans = vec![
        Span::raw(padded(&terminal_safe(row.name()), columns.skill)),
        // A label that places content with a registered source is body text.
        // A source name does that outright, and so do "mixed" and "multiple
        // sources": each reports at least one installation that resolved to
        // one. "not registered" and "unverified" place nothing with a source,
        // and are set back — the two are still different answers, and the
        // words keep them apart. The prototype mutes every source cell alike
        // (`.source-name`); this narrows that to the two that place nothing.
        match provenance {
            RowProvenance::Unregistered | RowProvenance::Unverified => {
                Span::styled(source, theme::pane_subtitle())
            }
            RowProvenance::NotApplicable
            | RowProvenance::Source(_)
            | RowProvenance::Mixed
            | RowProvenance::Divergent => Span::raw(source),
        },
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
    // `width` is the whole table region, not the width the capped columns
    // happen to use, so the selection band crosses the slack rather than
    // stopping where the health badge does.
    components::list_row(spans, selected, width)
}

fn installation_tone(health: InstallationHealth) -> Tone {
    match health {
        // Stray content is not a state of a skill, so it takes the inactive
        // tone; the words "not a skill" beside it carry the meaning.
        InstallationHealth::NotASkill => Tone::Inactive,
        InstallationHealth::Healthy => Tone::Healthy,
        // Unverified is in the unmanaged family: not known to be owned.
        InstallationHealth::Unverified | InstallationHealth::Unmanaged => Tone::Unmanaged,
        InstallationHealth::Broken => Tone::Critical,
    }
}

/// What an empty table can honestly say, given what the scan observed.
fn inventory_empty_state(app: &SkilledApp) -> (String, String) {
    let roots = app.inventory().roots();
    // Before any filter question: an unscanned snapshot holds nothing a
    // filter could have hidden, so "No skills match the filter" would promise
    // installed skills that were never read. Deselected roots may sit beside
    // pending ones without blocking this arm.
    if app.inventory().scan_pending() {
        return (
            "Installation roots have not been scanned".to_owned(),
            "Skilled scans the roots when this view opens.".to_owned(),
        );
    }
    // Nothing was looked at, so nothing may be said about what exists — and a
    // surviving filter must not invent installed skills to match against.
    if app.inventory().no_agent_configured() {
        return (
            "No agent is configured".to_owned(),
            "Skilled reads the skill root of the agents chosen during setup, \
             and none are chosen, so it read nothing. Rerun setup from \
             Settings to choose an agent."
                .to_owned(),
        );
    }
    if !app.inventory_filter().trim().is_empty() {
        return (
            "No skills match the filter".to_owned(),
            "Press Esc to clear the filter and show every installed skill again.".to_owned(),
        );
    }
    if roots
        .iter()
        .any(|root| matches!(root.status(), RootStatus::Unreadable { .. }))
    {
        return (
            "An agent skill root could not be read".to_owned(),
            "Skilled reports nothing from a root it could not read in full \
             rather than reporting part of it. Each root that failed names its \
             reason above."
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

/// The column of vertical rule that divides one workspace region from the
/// next.
fn render_region_separator(frame: &mut Frame<'_>, area: Rect) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled("│", theme::rule()));
            usize::from(area.height)
        ]),
        area,
    );
}

/// A workspace pane: its header, the rule that closes it, and the body left
/// for the pane's own content.
fn render_pane_scaffold(
    frame: &mut Frame<'_>,
    area: Rect,
    heading: &str,
    subtitle: &str,
    focused: bool,
) -> Rect {
    let [header, body] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(area);
    frame.render_widget(
        Paragraph::new(vec![
            pane_header(heading, subtitle, focused, header.width),
            components::rule(header.width),
        ]),
        header,
    );
    body
}

/// The gap `components::focused_pane_header` sets between a heading and its
/// subtitle. A components test holds the two together.
const SUBTITLE_GAP: usize = 2;

/// A pane header bounded to the pane it heads.
///
/// The subtitle quantifies the pane and can be a phrase rather than a count,
/// so it is bounded rather than left to run off the end of the header: a
/// status cut mid-word says neither what it is nor that there was more of it.
/// The heading names the pane and is never cut — a pane whose name did not fit
/// would have nothing left to say.
fn pane_header(heading: &str, subtitle: &str, focused: bool, width: u16) -> Line<'static> {
    let subtitle = bounded_subtitle(
        subtitle,
        usize::from(width)
            .saturating_sub(Span::raw(heading).width())
            .saturating_sub(if focused { ROW_MARKER_WIDTH } else { 0 })
            .saturating_sub(SUBTITLE_GAP),
    );
    components::focused_pane_header(heading, &subtitle, focused)
}

/// Bound a subtitle by shedding whole ` · ` clauses from its end, the way a
/// group label sheds its qualifiers: `scan error · 3 found` in a narrow pane
/// says `scan error`, not `scan erro...`, because the clauses lead with the
/// fact the pane cannot restate and a cut word destroys exactly that fact. A
/// shed clause leaves no mark; nothing on the row claims it was stated. Only
/// a first clause too long for the pane on its own is cut, ellipsized so the
/// row still says there was more.
fn bounded_subtitle(subtitle: &str, budget: usize) -> String {
    let safe = terminal_safe(subtitle);
    let mut clauses = safe.as_str();
    loop {
        if Span::raw(clauses).width() <= budget {
            return clauses.to_owned();
        }
        match clauses.rfind(" · ") {
            Some(cut) => clauses = &clauses[..cut],
            None => return terminal_safe_bounded_start(clauses, budget),
        }
    }
}

/// The detail region's frame, shared by every screen that has one.
///
/// Beside a primary region it opens with the dividing rule; drilled into on a
/// compact terminal it fills the workspace. Either way the surface is painted
/// whole, before the text margin, and the header and its rule sit inside —
/// the two screens' detail regions cannot drift apart because they are the
/// same scaffold. The body left for the caller's lines is returned.
fn render_detail_scaffold(
    frame: &mut Frame<'_>,
    area: Rect,
    heading: &str,
    subtitle: &str,
    focused: bool,
    beside_the_primary_region: bool,
) -> Rect {
    let region = if beside_the_primary_region {
        let [separator, region] =
            Layout::horizontal([Constraint::Length(1), Constraint::Min(1)]).areas(area);
        render_region_separator(frame, separator);
        // Painted whole, before the margin: the surface is what makes the
        // region read as a region, so it reaches the edges the text does not.
        frame.render_widget(Block::new().style(theme::detail_surface()), region);
        region.inner(Margin {
            horizontal: 1,
            vertical: 0,
        })
    } else {
        frame.render_widget(Block::new().style(theme::detail_surface()), area);
        area.inner(Margin {
            horizontal: 1,
            vertical: 0,
        })
    };
    render_pane_scaffold(frame, region, heading, subtitle, focused)
}

/// The detail region: everything observed about the selected installation.
fn render_inventory_detail(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &SkilledApp,
    beside_the_table: bool,
) {
    let selected = app.selected_installation();
    let body = render_detail_scaffold(
        frame,
        area,
        "Details",
        &selected.map_or_else(
            || "no selection".to_owned(),
            |row| terminal_safe(row.name()),
        ),
        app.inventory_pane() == InventoryPane::Details,
        beside_the_table,
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
    // The detail region is the only place per-agent observations and findings
    // exist, so content that does not fit is reported as missing rather than
    // dropped off the bottom without a trace.
    let lines = inventory_detail_lines(row, app.home(), body.width);
    frame.render_widget(
        Paragraph::new(bounded_detail_lines(lines, body.width, body.height))
            .wrap(Wrap { trim: false }),
        body,
    );
}

/// Fit detail lines to a region, saying so when some do not fit.
///
/// The last rows are spent on a count of what was left out, because a region
/// that silently ends mid-section reads as though there were nothing more. The
/// notice is measured like any other line and shortened rather than wrapped
/// off the bottom: the one string whose whole job is to report that content
/// was cut must not itself be cut.
fn bounded_detail_lines(lines: Vec<Line<'static>>, width: u16, height: u16) -> Vec<Line<'static>> {
    let available = usize::from(height);
    if width == 0 || available == 0 || detail_lines_height(&lines, width) <= available {
        return lines;
    }

    // Rows hidden, not lines hidden: a dropped line that would have wrapped
    // costs the reader more than one row of content.
    let total_rows = detail_lines_height(&lines, width);
    let notice = |hidden: usize| {
        let plural = if hidden == 1 { "" } else { "s" };
        [
            format!("{hidden} more line{plural} — widen or lengthen the terminal"),
            format!("{hidden} more line{plural}"),
            format!("+{hidden}"),
        ]
        .into_iter()
        .map(|label| Line::from(components::badge(Tone::Warning, &label)))
        .find(|line| wrapped_line_count(line, width) == 1)
        .unwrap_or_else(|| Line::from(components::badge(Tone::Warning, "…")))
    };

    let reserved = wrapped_line_count(&notice(total_rows), width);
    let mut kept = Vec::new();
    let mut used = 0;
    for line in &lines {
        let rows = wrapped_line_count(line, width);
        if used + rows > available.saturating_sub(reserved) {
            break;
        }
        kept.push(line.clone());
        used += rows;
    }
    kept.push(notice(total_rows - used));
    kept
}

fn inventory_detail_lines(row: &InventoryRow, home: &Path, width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    // Kicker, then the skill's own name as the title, then its health: the
    // badge words already say what they mean, so no field label repeats the
    // column headings the table has just shown. The pane header above still
    // names the same skill — that repetition is kept, because the header is
    // the focus contract and this title belongs to the section anatomy.
    push_detail_section(&mut lines, "SKILL", width);
    lines.push(Line::styled(
        terminal_safe(row.name()),
        theme::pane_heading(),
    ));
    lines.push(Line::from(components::badge(
        installation_tone(row.health()),
        row.health().label(),
    )));
    if let Some(SkillValidation::Valid { description, .. }) = row
        .observations()
        .find_map(InstalledSkillObservation::validation)
    {
        lines.push(detail_field_bounded("Description", description, width, 3));
    }

    push_detail_section(&mut lines, "SOURCE", width);
    match row.provenance() {
        RowProvenance::Source(label) => lines.push(detail_field("Source", label)),
        // Naming one of them would misstate the other, so each agent's section
        // below names its own.
        RowProvenance::Divergent => lines.push(Line::from(
            "Installed from more than one registered source; each agent names its own below.",
        )),
        // A source line would claim the installation that resolved to none.
        RowProvenance::Mixed => lines.push(Line::from(
            "Registered for some agents but not others; each agent's section below says which.",
        )),
        // Not knowing where something came from is not the same as knowing it
        // came from nowhere registered.
        RowProvenance::Unverified => lines.push(Line::from(
            "A registered source could not be read, so Skilled cannot tell whether \
             this came from one.",
        )),
        // Unresolved content is observed, never adopted.
        RowProvenance::Unregistered => lines.push(Line::from(
            "Not resolved to any registered source; Skilled does not manage it.",
        )),
        // Stray content was never installed from anywhere, so no source
        // question applies to it.
        RowProvenance::NotApplicable => lines.push(Line::from(
            "Not a skill installation, so no source applies.",
        )),
    }

    for observation in row.observations() {
        // The row's own health is a rollup across the agents; each section
        // states the one installation it describes.
        push_detail_section_badge(
            &mut lines,
            &observation.agent().display_name().to_uppercase(),
            components::badge(
                installation_tone(observation.health()),
                observation.health().label(),
            ),
            width,
        );
        lines.extend(observation_lines(
            observation,
            home,
            width,
            matches!(
                row.provenance(),
                RowProvenance::Divergent | RowProvenance::Mixed
            ),
        ));
    }

    // The agents that carry nothing share one line rather than three empty
    // sections, so the observations that exist keep the room.
    let absent: Vec<&str> = AgentKind::ALL
        .into_iter()
        .filter(|agent| row.observation(*agent).is_none())
        .map(AgentKind::display_name)
        .collect();
    if !absent.is_empty() {
        push_detail_section(&mut lines, "NOT INSTALLED", width);
        lines.push(Line::from(components::badge(
            Tone::Inactive,
            &absent.join(", "),
        )));
    }
    lines
}

fn observation_lines(
    observation: &InstalledSkillObservation,
    home: &Path,
    width: u16,
    name_its_source: bool,
) -> Vec<Line<'static>> {
    // Findings come first. A region too short to hold the section truncates
    // from the bottom, and the reason an installation is broken is the thing
    // this view exists to report — losing it to a `Path` the table's own name
    // column already implies would be the wrong trade.
    let mut lines: Vec<Line<'static>> = observation
        .findings()
        .iter()
        .flat_map(|finding| finding_lines(finding, width))
        .collect();
    lines.push(detail_field_bounded(
        "Path",
        &home_relative(observation.path(), home),
        width,
        2,
    ));
    lines.push(detail_field("Object", observation.object().description()));
    if let Some(resolution) = observation.resolution() {
        // Named here only when the agents disagree; otherwise the row's own
        // SOURCE section above has already said it once.
        if name_its_source {
            lines.push(detail_field("Source", resolution.source_label()));
        }
        lines.push(detail_field_bounded(
            "Variant",
            &format!(
                "{} · {}",
                resolution.catalog_relative_path().display(),
                resolution.variant_relative_path().display()
            ),
            width,
            2,
        ));
    }
    if let InstallationObject::Symlink { target } = observation.object() {
        // An unreadable link renders as an empty target; an empty value beside
        // a label reads as "nothing there" rather than "not known".
        let target = if target.as_os_str().is_empty() {
            "could not be read".to_owned()
        } else {
            home_relative(target, home)
        };
        lines.push(detail_field_bounded("Target", &target, width, 2));
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

/// The Repositories pane's share of a wide primary region, matching the
/// prototype's fixed 270px column at roughly eight pixels a cell.
///
/// The cap is what makes the workspace's wide-detail crossing
/// ([`viewport::DETAIL_REGION_WIDE_THRESHOLD`]) cost this pane nothing: it
/// binds from a primary region of 81 columns, and the crossing takes the
/// primary from 110 columns to 101, so the pane is 34 either side of it and
/// every repository entry is laid out identically. Below 81 the share is
/// proportional, because a pane that took its full 34 out of a narrow primary
/// would leave the variants beside it too little to read.
const REPOSITORIES_PANE_MAX_WIDTH: u16 = 34;

fn repositories_pane_width(primary_width: u16) -> u16 {
    // Saturation is only reachable far above the cap, so it cannot change the
    // share a real terminal is given.
    (primary_width.saturating_mul(42) / 100).min(REPOSITORIES_PANE_MAX_WIDTH)
}

/// Past this a variant name stops earning width, exactly as a skill name does
/// in the inventory table; the detail region beside the list states the name
/// on its own bounded line, so a name too long for that line is elided there
/// too.
const MAX_VARIANT_WIDTH: usize = MAX_SKILL_WIDTH;

/// The widest content the variants pane lays out.
///
/// This is what the pane keeps on the far side of the wide-detail crossing:
/// at 151 columns the primary region is 101, less 34 for the Repositories
/// pane, one for the rule between them and one for the gutter after it.
/// Bounding the pane's content here rather than at its own width means
/// widening the terminal past the threshold takes the columns out of slack —
/// which the group label's band and a selected row's band still cross — and
/// never out of a catalog path or a variant name that was readable a column
/// earlier. Beside the detail region that slack stays empty, for the same
/// reason: [`group_label`] sets its qualifiers against the pane's right edge
/// only in the compact viewport, where widening always widens the pane —
/// up to the relayout at [`viewport::WIDE_MINIMUM_WIDTH`], which no pane
/// content survives whole in any case.
const VARIANTS_CONTENT_MAX_WIDTH: usize = 65;

fn render_sources(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp) {
    match viewport::workspace_regions(area) {
        (primary, Some(details)) => {
            // A region that opens on a rule is set in from it, the way the
            // detail region beside it is: the gutter belongs to the rule, not
            // to the pane, so a region at the screen edge keeps none.
            let [repositories, separator, _gutter, variants] = Layout::horizontal([
                Constraint::Length(repositories_pane_width(primary.width)),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(1),
            ])
            .areas(primary);
            render_region_separator(frame, separator);
            render_source_repositories(frame, repositories, app);
            render_source_variants(frame, variants, app, true);
            render_source_details(frame, details, app, true);
        }
        (primary, None) => match app.sources_pane() {
            SourcesPane::Repositories => render_source_repositories(frame, primary, app),
            SourcesPane::Variants => render_source_variants(frame, primary, app, false),
            SourcesPane::Details => render_source_details(frame, primary, app, false),
        },
    }
}

fn render_source_repositories(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp) {
    let inner = render_pane_scaffold(
        frame,
        area,
        "Repositories",
        &format!("{} registered", app.sources().len()),
        app.sources_pane() == SourcesPane::Repositories,
    );

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

    // Entries are three lines tall, so the pane holds a third as many of them
    // as it has rows. A pane too short for one still shows the top of the
    // focused entry rather than nothing.
    let capacity = (usize::from(inner.height) / REPOSITORY_ENTRY_LINES).max(1);
    let start = visible_window_start(app.focused_source(), capacity);
    let lines = app
        .sources()
        .iter()
        .enumerate()
        .skip(start)
        .take(capacity)
        .flat_map(|(index, source)| {
            repository_entry_lines(source, index == app.focused_source(), inner.width)
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// How many lines one repository entry occupies.
const REPOSITORY_ENTRY_LINES: usize = 3;

/// One registered repository, in the prototype's `.source-row` anatomy: what
/// the source is called, the checkout it names, and the state it was last seen
/// in.
///
/// Every line is bounded to the pane rather than wrapped. A wrapped path would
/// push the state line of one entry into the next and leave the list without a
/// fixed entry height, so the row could no longer be windowed or banded; the
/// detail region beside the list still gives the path in full.
///
/// The path is muted where the prototype's `.source-path` is faint: it names
/// the checkout this entry stands for, which is information and has to meet
/// 4.5:1, the same reason the inventory table's column headings are muted
/// rather than faint.
fn repository_entry_lines(
    source: &RegisteredSource,
    selected: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let budget = usize::from(width).saturating_sub(ROW_MARKER_WIDTH);
    let state = source_status_badge(source);
    let state_width = state.width();
    let revision = format!(
        " {}@{}",
        terminal_safe(source.branch().unwrap_or("detached")),
        terminal_safe(source.short_head())
    );
    components::list_row_lines(
        vec![
            vec![Span::raw(terminal_safe_bounded_start(
                source.label(),
                budget,
            ))],
            vec![Span::styled(
                terminal_safe_bounded_middle(&source.git_top_level().display().to_string(), budget),
                theme::pane_subtitle(),
            )],
            vec![
                state,
                Span::styled(
                    terminal_safe_bounded_start(&revision, budget.saturating_sub(state_width)),
                    theme::pane_subtitle(),
                ),
            ],
        ],
        selected,
        width,
    )
}

/// `beside_details` says whether the detail region is on screen beside the
/// pane; a group label spends the pane's slack only when it is not (see
/// [`group_label`]).
fn render_source_variants(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &SkilledApp,
    beside_details: bool,
) {
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
        // The failure leads, so a subtitle bounded to a narrow pane gives up
        // the count rather than the warning.
        Some(_) if catalog_error_count > 0 => format!("scan error · {} found", variants.len()),
        Some(_) => format!("{} found", variants.len()),
        None => "no source".to_owned(),
    };
    let inner = render_pane_scaffold(
        frame,
        area,
        "Available variants",
        &subtitle,
        app.sources_pane() == SourcesPane::Variants,
    );

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

    // Reserved for a source with no catalogs at all: a catalog that scanned
    // clean but holds nothing is still named by the grouping below, with the
    // `no variants` line beneath its label, because flattening it into this
    // state would hide which catalogs the source has and that each was read.
    // Registration refuses a source with no included catalog, so this is a
    // defensive fallback — and it says nowhere to look, not nothing found,
    // because a source with no roots was never scanned at all.
    if source.catalogs().is_empty() {
        frame.render_widget(
            components::empty_state(
                "·",
                "No catalog roots registered",
                "This source has no confirmed catalog roots, so there was \
                 nowhere to look for variants.",
                inner,
            ),
            inner,
        );
        return;
    }

    // Each catalog states its own path once, above the variants it holds, and
    // keeps its own scan failure beneath that label: an error stacked above
    // the whole list would not say which catalog could not be read.
    //
    // The focused line, and the label each line belongs under, are recorded as
    // the lines are built rather than computed from the selection, because
    // group labels and error lines sit between the rows and only this loop
    // knows where they fell.
    // Every rendered row is a focus position — each candidate, and each
    // catalog's state row (its error, or `no variants`) — so
    // `move_sources_selection` counts exactly these rows, the window follows
    // the selection, and a list taller than the pane can be walked whatever
    // mixture of skills, errors, and empty catalogs it holds. `selected_row`
    // walks the same order to answer what the selection rests on.
    let mut lines = Vec::new();
    let mut group_labels = Vec::new();
    let mut focused_line = 0;
    let mut position = 0;
    for catalog in source.catalogs() {
        let label = lines.len();
        lines.push(catalog_group_label(catalog, inner.width, beside_details));
        group_labels.push(label);
        if let Some(error) = catalog.scan_error() {
            let selected = position == app.focused_variant();
            if selected {
                focused_line = lines.len();
            }
            let badge = components::badge(Tone::Critical, "unavailable");
            // Bounded to the pane like every other row: a wrapped error would
            // put the marker and the band on one line and the words on the
            // next. The detail region gives the message more room — three
            // bounded lines — but a message past those is elided there too.
            let budget = usize::from(inner.width)
                .min(VARIANTS_CONTENT_MAX_WIDTH)
                .saturating_sub(ROW_MARKER_WIDTH + badge.width() + 1);
            lines.push(components::list_row(
                vec![
                    badge,
                    Span::raw(format!(" {}", terminal_safe_bounded_start(error, budget))),
                ],
                selected,
                inner.width,
            ));
            group_labels.push(label);
            position += 1;
        }
        for candidate in catalog.candidates() {
            let selected = position == app.focused_variant();
            if selected {
                focused_line = lines.len();
            }
            lines.push(variant_row(candidate, selected, inner.width));
            group_labels.push(label);
            position += 1;
        }
        if catalog.candidates().is_empty() && catalog.scan_error().is_none() {
            // Said rather than left blank: two labels in a row would read as
            // though the rows under the first belonged to both, and a label
            // with nothing under it would not say whether the catalog is empty
            // or the list has scrolled.
            let selected = position == app.focused_variant();
            if selected {
                focused_line = lines.len();
            }
            lines.push(components::list_row(
                vec![Span::styled(
                    "no variants".to_owned(),
                    theme::pane_subtitle(),
                )],
                selected,
                inner.width,
            ));
            group_labels.push(label);
            position += 1;
        }
    }
    debug_assert_eq!(
        position,
        app.variants_row_count(),
        "the pane renders exactly the rows the selection counts"
    );
    if variants.is_empty() && catalog_error_count > 0 {
        // The hint belongs to catalog errors: an all-empty source that read
        // cleanly has nothing further for Details to explain. It travels
        // with the last group and scrolls as the rows do.
        lines.push(Line::from("Open Details for the catalog error."));
        group_labels.push(group_labels.last().copied().unwrap_or(0));
    }
    debug_assert_eq!(
        lines.len(),
        group_labels.len(),
        "every line belongs under a label"
    );

    let visible = visible_grouped_lines(
        &lines,
        &group_labels,
        focused_line,
        inner.width,
        usize::from(inner.height),
    );
    frame.render_widget(Paragraph::new(visible).wrap(Wrap { trim: false }), inner);
}

/// One variant: its validation state and the directory it lives in.
///
/// The directory name alone, because the catalog above the row already gives
/// the path it sits in and the detail region gives the path in full.
fn variant_row(candidate: &SkillCandidate, selected: bool, width: u16) -> Line<'static> {
    let valid = candidate.validation().is_valid();
    let state = components::badge(
        if valid { Tone::Healthy } else { Tone::Critical },
        if valid { "valid" } else { "invalid" },
    );
    // Bounded to the pane as well as to the name cap. A row wide enough to
    // wrap would leave its marker and the head of its band on one line and the
    // name they identify on the next, which is the one thing the marker is
    // there to say.
    let budget = usize::from(width)
        .saturating_sub(ROW_MARKER_WIDTH + state.width() + 1)
        .min(MAX_VARIANT_WIDTH);
    components::list_row(
        vec![
            state,
            Span::raw(format!(
                " {}",
                terminal_safe_bounded_start(candidate.directory_name(), budget)
            )),
        ],
        selected,
        width,
    )
}

/// The line naming the catalog a run of variants belongs to (prototype
/// `.catalog-title`): where it is, and as much of how it is classified and
/// which agents it is registered for as the label has room for.
fn catalog_group_label(
    catalog: &CatalogProposal,
    width: u16,
    beside_details: bool,
) -> Line<'static> {
    group_label(
        &catalog.relative_path().display().to_string(),
        catalog_classification(catalog),
        &compatibility_claim(catalog.compatibility()),
        width,
        beside_details,
    )
}

/// The least gap between a group label's path and its qualifiers.
///
/// Set against the right edge, the qualifiers need enough space that they read
/// as a separate statement about the catalog and not as the tail of its path —
/// which is exactly what a single space, the gap inside `Common · all agents`,
/// would make them. Two is also where the prototype's `.catalog-title` lands:
/// its `gap: 12px` is a cell and a half at the eight pixels a cell the other
/// bounds here are read at, and half a column is not spendable.
const GROUP_LABEL_QUALIFIER_GAP: usize = 2;

/// A group label: the path at the left, and whichever qualifiers the pane can
/// hold whole against its right edge.
///
/// The qualifiers describe the catalog; the path is which catalog, and the
/// rows beneath the label no longer carry that themselves. So the path is laid
/// out first, bounded by [`VARIANTS_CONTENT_MAX_WIDTH`] like every other piece
/// of content in the pane, and the qualifiers take the slack past it. A
/// qualifier is added only once it costs the path nothing, and dropped in the
/// order that gives up the least: the classification goes first, because a
/// claim of every agent or of one named agent is the more specific fact and
/// the one a reader scanning for their own agent is looking for.
///
/// Chosen this way, widening the pane can only ever add: the path shown is
/// `min(path, cap, width)`, which never shrinks, and every column past it is a
/// column the label may spend — on the qualifiers where they may have it, and
/// otherwise on the blank its band crosses. That is the same promise
/// [`viewport::DETAIL_REGION_WIDE_THRESHOLD`] makes for the inventory table's
/// columns — a name readable at one width must not be ellipsized at the next
/// one up. The promise is the pane's, not the terminal's: widening the
/// terminal from 99 columns to 100 relays the whole workspace, taking the
/// variants pane from the full width to a third of it, and no piece of pane
/// content survives that crossing whole.
///
/// A shed qualifier leaves no mark, where a shortened path leaves an ellipsis.
/// That is deliberate: the qualifiers are a description of the catalog, and
/// both facts are given in full in the detail region under CATALOG for the
/// catalog the selection rests in, so a narrow pane is not the reader's only
/// account of them. Nothing on the line claims they were stated.
///
/// This follows the prototype's `.catalog-title`, which sets path and
/// qualifiers `space-between`. The objection recorded when they were first set
/// adjacent — that the pane's slack is what a selected row's band crosses, so
/// a label spanning it would read as two columns the rows beneath do not have
/// — is answered by that band: it already crosses the same slack, and the
/// label's own band crosses it whether or not a word sits at the far end.
///
/// What splitting them buys is the shedding the cap would otherwise force on a
/// pane far wider than the cap. That is only the compact viewport, where the
/// variants pane is the whole workspace and no detail region is on screen at
/// all, so `beside_details` is where the qualifiers stop: beside the detail
/// region they end at the content cap like everything else in the pane.
/// Spending that slack there would cost more than it gave — the variants pane
/// is 74 columns at a terminal of 150 and 65 at 151, so a qualifier held only
/// by the slack would vanish as the terminal widened past the very crossing
/// [`viewport::DETAIL_REGION_WIDE_THRESHOLD`] and the cap exist to survive.
fn group_label(
    path: &str,
    classification: &str,
    claim: &str,
    width: u16,
    beside_details: bool,
) -> Line<'static> {
    let width = usize::from(width);
    // The path is bounded to the pane's content cap; the qualifiers are what
    // the slack past it is spent on, and only where that slack is stable.
    let content = width.min(VARIANTS_CONTENT_MAX_WIDTH);
    let path = terminal_safe_bounded_middle(path, content);
    let path_width = Span::raw(&path).width();
    let qualifiers_end = if beside_details { content } else { width };
    let qualifiers = [format!("{classification} · {claim}"), claim.to_owned()]
        .into_iter()
        .find(|qualifiers| {
            path_width
                .saturating_add(GROUP_LABEL_QUALIFIER_GAP)
                .saturating_add(Span::raw(qualifiers).width())
                <= qualifiers_end
        })
        .unwrap_or_default();
    // The qualifiers are set flush against the end they were fitted to, and
    // the line is then padded to the pane: its band crosses the whole region,
    // the way a selected row's band does, and is a single row.
    let qualifiers_width = Span::raw(&qualifiers).width();
    let gap = qualifiers_end
        .saturating_sub(path_width)
        .saturating_sub(qualifiers_width);
    let padding = width
        .saturating_sub(path_width)
        .saturating_sub(gap)
        .saturating_sub(qualifiers_width);
    Line::styled(
        format!(
            "{path}{}{qualifiers}{}",
            " ".repeat(gap),
            " ".repeat(padding)
        ),
        theme::group_label(),
    )
}

/// Which agents a catalog is registered for, for the variants group label and
/// the detail region's CATALOG section alike.
///
/// Skilled proposes this from the catalog's place in the checkout and the user
/// confirms or edits it; the catalog itself declares nothing and no agent was
/// asked. So the phrase names what is stored and nothing more. A catalog
/// registered for none says so rather than rendering an empty phrase, and one
/// registered for some names those and stops: the agents left out are the ones
/// not claimed, which is what the setup dialog's exhaustive yes/no list is for.
fn compatibility_claim(compatibility: Compatibility) -> String {
    if compatibility.all_supported() {
        return "all agents".to_owned();
    }
    let claimed = AgentKind::ALL
        .into_iter()
        .filter(|agent| compatibility.supports(*agent))
        .map(AgentKind::display_name)
        .collect::<Vec<_>>();
    if claimed.is_empty() {
        return "no agents".to_owned();
    }
    claimed.join(" + ")
}

/// A stored scan time as `YYYY-MM-DD HH:MM UTC`.
///
/// The civil date is computed here rather than taken from a date crate, which
/// would be a production dependency for one field. The algorithm is Howard
/// Hinnant's `civil_from_days`. The two divisions that can be handed a
/// negative value — the split into days and seconds here, and the split into
/// eras below — are Euclidean, so a time before the epoch names the day it
/// falls on instead of rounding towards zero into the day after it; every
/// division downstream of those is given a non-negative operand by
/// construction. UTC is named in the text because nothing converts the value
/// to the reader's zone.
fn format_scan_timestamp(seconds: i64) -> String {
    // Split first, so the time of day is taken from the remainder of the day
    // the date names — negative seconds included.
    const SECONDS_PER_DAY: i64 = 86_400;
    let days = seconds.div_euclid(SECONDS_PER_DAY);
    let second_of_day = seconds.rem_euclid(SECONDS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    let hour = second_of_day / 3_600;
    let minute = (second_of_day % 3_600) / 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02} UTC")
}

/// The civil date `days` after 1970-01-01, by Hinnant's algorithm: shift the
/// epoch to a 400-year era beginning on 1 March, so the leap day is the last
/// day of the era's year and every other month has a fixed length.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    const DAYS_FROM_0000_03_01_TO_EPOCH: i64 = 719_468;
    const DAYS_PER_ERA: i64 = 146_097;

    let shifted = days + DAYS_FROM_0000_03_01_TO_EPOCH;
    let era = shifted.div_euclid(DAYS_PER_ERA);
    // Day of the era, and from here every operand is non-negative.
    let day_of_era = shifted - era * DAYS_PER_ERA;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    // Month counted from March, which is what makes the lengths regular.
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = shifted_month + if shifted_month < 10 { 3 } else { -9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn render_source_details(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &SkilledApp,
    beside_the_primary_region: bool,
) {
    let selected = selected_variant(app);
    let subtitle = selected
        .map(|variant| terminal_safe(variant.candidate.directory_name()))
        .or_else(|| {
            app.selected_source()
                .map(|source| terminal_safe(source.label()))
        })
        .unwrap_or_else(|| "no selection".to_owned());
    let inner = render_detail_scaffold(
        frame,
        area,
        "Details",
        &subtitle,
        app.sources_pane() == SourcesPane::Details,
        beside_the_primary_region,
    );

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
    repository_lines.push(detail_field_middle(
        "Path",
        &source.git_top_level().display().to_string(),
        inner.width,
    ));
    // The abbreviation Git prints, not the whole revision: forty characters
    // outrun the narrow tier of this region, and a value wrapped below its
    // label can be cut away from it by the row budget.
    repository_lines.push(detail_field("HEAD", source.short_head()));
    repository_lines.push(detail_field_middle(
        "Remote",
        source.remote_url().unwrap_or("not configured"),
        inner.width,
    ));
    // The scan time shares the status line while both stand on it whole, and
    // takes the line below where they do not — the way the catalog states its
    // classification. Left to wrap it outran both aside tiers, and what landed
    // on the row beneath was the whole timestamp at the narrow one and `UTC`
    // alone at the wide one: a time naming no zone, under a label naming no
    // value. Sharing where it fits keeps the row for the sections below, which
    // the drill-in has no aside to spare.
    let status_label = Span::styled("Status: ", theme::pane_subtitle());
    let status = source_status_badge(source);
    let separator = Span::raw(" · ");
    let scan_label = Span::styled("Last scan: ", theme::pane_subtitle());
    let scanned = Span::raw(format_scan_timestamp(source.last_scan_at()));
    let shared = status_label.width()
        + status.width()
        + separator.width()
        + scan_label.width()
        + scanned.width();
    if shared <= usize::from(inner.width) {
        repository_lines.push(Line::from(vec![
            status_label,
            status,
            separator,
            scan_label,
            scanned,
        ]));
    } else {
        repository_lines.push(Line::from(vec![status_label, status]));
        repository_lines.push(Line::from(vec![scan_label, scanned]));
    }
    if let Some(error) = source.source_error() {
        repository_lines.push(detail_field_bounded("Source error", error, inner.width, 3));
    }

    let mut catalog_lines = Vec::new();
    push_detail_section(&mut catalog_lines, "CATALOG", inner.width);
    // The catalog of whichever row the selection rests on — a state row names
    // its catalog as surely as a variant does, so moving the band in the
    // variants pane always changes what this section says.
    if let Some(catalog) = selected_catalog(app) {
        // The classification shares the path's line while the path is whole
        // beside it. It is never cut to make room, and never crowds the path
        // down to an elision short enough to pass for a path of its own: where
        // the two do not fit, the classification states itself on the line
        // below. That is where this parts company with the variants group
        // label, which sheds a qualifier it cannot fit — a pane row has one
        // line to give and this region has another to spend, so the fact is
        // moved here rather than dropped.
        let path = terminal_safe(&catalog.relative_path().display().to_string());
        let classification = format!(" · Classification: {}", catalog_classification(catalog));
        let budget = detail_value_budget("Path", inner.width);
        if Span::raw(&path).width() + Span::raw(&classification).width() <= budget {
            catalog_lines.push(detail_field("Path", &format!("{path}{classification}")));
        } else {
            catalog_lines.push(detail_field_middle("Path", &path, inner.width));
            catalog_lines.push(detail_field(
                "Classification",
                catalog_classification(catalog),
            ));
        }
        // The same phrase the variants group label uses, so the region and the
        // pane beside it name a catalog's claim in one vocabulary.
        catalog_lines.push(detail_field_bounded(
            "Compatibility",
            &compatibility_claim(catalog.compatibility()),
            inner.width,
            1,
        ));
    } else {
        catalog_lines.push(Line::from(
            "No catalog selected; catalog metadata is unavailable.",
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
        // One bounded line, not the name in full: in the narrow aside tier
        // the directory and the declared name together have fewer cells than
        // the variants pane's cap alone allows a row. What the region adds is
        // the second fact — the name the skill declares beside the directory
        // it lives in — which a pane row has no room to name at all.
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
        // Trailing elision, not the middle one Repository Path and Remote
        // take: what a trailing cut loses here is the directory name the
        // field one row above has just stated, where the middle of the path
        // is restated by no neighbouring field.
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

/// What the variants-pane selection rests on: a variant, or a catalog's
/// state row (its error, or `no variants`).
enum SelectedSourceRow<'a> {
    Variant(SourceVariant<'a>),
    CatalogState(&'a CatalogProposal),
}

/// The row the selection rests on, walked in the pane's render order: for
/// each catalog its error row, then its candidates, then its `no variants`
/// row. `render_source_variants` builds exactly these rows and asserts the
/// count against `variants_row_count`, which counts them the same way.
fn selected_row(app: &SkilledApp) -> Option<SelectedSourceRow<'_>> {
    let source = app.selected_source()?;
    // An unavailable source renders its source error and no rows, so nothing
    // is selected — mirroring `variants_row_count`, which counts none.
    if source.source_error().is_some() {
        return None;
    }
    let mut position = 0;
    for catalog in source.catalogs() {
        if catalog.scan_error().is_some() {
            if position == app.focused_variant() {
                return Some(SelectedSourceRow::CatalogState(catalog));
            }
            position += 1;
        }
        for candidate in catalog.candidates() {
            if position == app.focused_variant() {
                return Some(SelectedSourceRow::Variant(SourceVariant {
                    catalog,
                    candidate,
                }));
            }
            position += 1;
        }
        if catalog.candidates().is_empty() && catalog.scan_error().is_none() {
            if position == app.focused_variant() {
                return Some(SelectedSourceRow::CatalogState(catalog));
            }
            position += 1;
        }
    }
    None
}

fn selected_variant(app: &SkilledApp) -> Option<SourceVariant<'_>> {
    match selected_row(app)? {
        SelectedSourceRow::Variant(variant) => Some(variant),
        SelectedSourceRow::CatalogState(_) => None,
    }
}

/// The catalog the selection rests in, whichever row kind carries it: a
/// selected state row names its catalog as surely as a selected variant does,
/// so the Details CATALOG section follows the band across the region
/// boundary instead of rendering identically for every position.
fn selected_catalog(app: &SkilledApp) -> Option<&CatalogProposal> {
    match selected_row(app)? {
        SelectedSourceRow::Variant(variant) => Some(variant.catalog),
        SelectedSourceRow::CatalogState(catalog) => Some(catalog),
    }
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
    push_detail_section_line(
        lines,
        Line::styled(title.to_owned(), theme::detail_section_title()),
        width,
    );
}

/// A detail section whose heading carries a status of its own.
///
/// The prototype tones the whole section by colouring its left border; a
/// terminal has no border to tone, so the badge sits in the heading.
fn push_detail_section_badge(
    lines: &mut Vec<Line<'static>>,
    title: &str,
    badge: Span<'static>,
    width: u16,
) {
    push_detail_section_line(
        lines,
        Line::from(vec![
            Span::styled(title.to_owned(), theme::detail_section_title()),
            Span::raw("  "),
            badge,
        ]),
        width,
    );
}

fn push_detail_section_line(lines: &mut Vec<Line<'static>>, heading: Line<'static>, width: u16) {
    lines.push(heading);
    lines.push(components::rule(width));
}

fn detail_field(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}: "), theme::pane_subtitle()),
        Span::raw(terminal_safe(value)),
    ])
}

/// A field whose value has nowhere to wrap: a path, a remote URL. Wrapped, it
/// breaks mid-word and reads as two values, and the row budget can cut the
/// continuation away from its label. Bounded to one line in the middle, both
/// ends of it survive instead — given a line with room for them. A budget too
/// narrow to hold anything either side of the ellipsis leaves the ellipsis
/// alone, which for a path would read as a path; no detail region is that
/// narrow, and a unit test holds the floor where it is.
fn detail_field_middle(label: &str, value: &str, width: u16) -> Line<'static> {
    detail_field(
        label,
        &terminal_safe_bounded_middle(value, detail_value_budget(label, width)),
    )
}

/// What one line of `width` leaves a field's value once its label is set.
fn detail_value_budget(label: &str, width: u16) -> usize {
    usize::from(width).saturating_sub(Span::raw(format!("{label}: ")).width())
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

/// The first line of the window that keeps `focused` visible: as far back as
/// the region has room for, so the focused line is read in the context above
/// it rather than pinned to the top of the region.
fn visible_wrapped_start(
    lines: &[Line<'static>],
    focused: usize,
    width: u16,
    height: usize,
) -> Option<usize> {
    let focused_line = lines.get(focused)?;
    if width == 0 || height == 0 {
        return None;
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
    Some(start)
}

fn wrapped_lines_from(
    lines: &[Line<'static>],
    start: usize,
    width: u16,
    height: usize,
) -> Vec<Line<'static>> {
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

/// The window of a grouped list, with the label of the group it opens inside
/// pinned to its first row.
///
/// `group_labels` gives, for every line, the line that labels its group.
/// Scrolled deep into a long group the label would otherwise sit above the
/// window, and rows read without it name a variant without naming the catalog
/// it came from — the question the per-row path used to answer. The pinned
/// label costs the window a row, so the window is measured again against what
/// is left rather than pushing the focused line off the bottom.
fn visible_grouped_lines(
    lines: &[Line<'static>],
    group_labels: &[usize],
    focused: usize,
    width: u16,
    height: usize,
) -> Vec<Line<'static>> {
    let label_above = |start: usize| {
        group_labels
            .get(start)
            .copied()
            .filter(|label| *label < start)
    };

    // Paying for a label can move the window forward into a group whose own
    // label costs something else, so the window and the label it pins are
    // settled together.
    let mut budget = height;
    loop {
        let Some(start) = visible_wrapped_start(lines, focused, width, budget) else {
            return Vec::new();
        };
        let Some(label) = label_above(start) else {
            // The window opens on a label of its own; nothing to pin above it.
            return wrapped_lines_from(lines, start, width, height);
        };
        let Some(remaining) = height
            .checked_sub(wrapped_line_count(&lines[label], width))
            .filter(|rows| *rows > 0)
        else {
            // No room to pin one; the rows themselves are what the reader
            // came for.
            return wrapped_lines_from(lines, start, width, height);
        };
        if remaining >= budget {
            // This label costs no more than the window has already given up,
            // so the two agree and it can be pinned — unless the rows
            // themselves need every row of the region, in which case they
            // keep it and the label stays where it is.
            let body = wrapped_lines_from(lines, start, width, remaining);
            let rows = body
                .iter()
                .map(|line| wrapped_line_count(line, width))
                .sum::<usize>();
            if rows <= remaining {
                let mut visible = vec![lines[label].clone()];
                visible.extend(body);
                return visible;
            }
            return wrapped_lines_from(lines, start, width, height);
        }
        // Strictly smaller every pass, and a budget of nothing has already
        // returned above, so the loop settles.
        budget = remaining;
    }
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
            if inventory_can_move_selection(app) {
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
            if app.can_filter_inventory() {
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
    // The band reaches the full width, so the row reads as chrome rather than
    // as a smear the length of the hints. The hint line itself only sets
    // foreground colours, apart from the key caps' own emphasis.
    frame.render_widget(Block::new().style(theme::chrome_band()), area);
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
/// installation, updates, repair, uninstall, and forget — are absent by
/// construction.
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
            if inventory_can_move_selection(app) {
                hints.push(KeyHint::new("j/k", "Move"));
            }
            if inventory_can_advance(app) {
                hints.push(KeyHint::essential("Enter", "Open"));
            }
            if app.can_filter_inventory() {
                hints.push(KeyHint::new("/", "Filter"));
            }
            hints.extend([
                KeyHint::new("2", "Sources"),
                KeyHint::new("s", "Settings"),
                KeyHint::new("?", "Help"),
                // The Inventory is where the application opens, so it is the
                // one view a user can reach without having passed a quit hint
                // on the way. It survives a narrow row.
                KeyHint::essential("q", "Quit"),
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

/// Selection only moves in the list region, and only when there is somewhere
/// to move it to.
fn inventory_can_move_selection(app: &SkilledApp) -> bool {
    app.inventory_pane() == InventoryPane::Skills && app.filtered_installation_count() > 1
}

/// Back unwinds an applied filter, then a drilled-in detail region.
fn inventory_can_go_back(app: &SkilledApp) -> bool {
    !app.inventory_filter().is_empty() || app.inventory_pane() == InventoryPane::Details
}

fn sources_can_move_selection(app: &SkilledApp) -> bool {
    match app.sources_pane() {
        SourcesPane::Repositories => app.sources().len() > 1,
        // Rows, not variants: catalog-state rows are focus positions too, and
        // a hint must appear exactly when the binding does something.
        SourcesPane::Variants => app.variants_row_count() > 1,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// What the narrowest detail region leaves its text: the region less the
    /// column of rule that opens it, less the margin either side.
    const NARROWEST_INNER_WIDTH: u16 = viewport::DETAIL_REGION_WIDTH - 1 - 2;

    fn label_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    /// A subtitle that does not fit sheds its trailing ` · ` clauses whole
    /// before any word is cut: `scan error · 3 found` in a narrow pane says
    /// `scan error`, never `scan erro...`, because a cut word says neither
    /// what it was nor that there was more of it. Only a first clause too
    /// long for the pane on its own falls back to the ellipsized cut.
    #[test]
    fn a_bounded_subtitle_sheds_whole_clauses_before_cutting_a_word() {
        let heading = "Available variants";
        let width = |budget: usize| {
            u16::try_from(Span::raw(heading).width() + SUBTITLE_GAP + budget)
                .expect("test widths fit a terminal")
        };
        let subtitle = |budget: usize| {
            label_text(&pane_header(
                heading,
                "scan error · 3 found",
                false,
                width(budget),
            ))
        };

        // Room for the whole phrase: nothing is shed.
        assert!(
            subtitle(20).ends_with("scan error · 3 found"),
            "{:?}",
            subtitle(20)
        );
        // Room for the warning but not the count: the count is shed whole
        // rather than the warning cut mid-word.
        assert!(subtitle(10).ends_with("scan error"), "{:?}", subtitle(10));
        assert!(!subtitle(10).contains("..."), "{:?}", subtitle(10));
        // Not even room for the warning: the ellipsized cut remains, so the
        // row still says there was more.
        assert!(subtitle(9).ends_with("scan e..."), "{:?}", subtitle(9));
    }

    /// The label is padded to the pane so its band crosses the region, which
    /// only reads as a band if it is one row of exactly that width.
    #[test]
    fn a_group_label_fills_its_pane_exactly_and_never_overruns_it() {
        for width in 0..=90_u16 {
            for path in [
                "skills",
                "experimental/nested/claude-code/skills",
                &"very-long-segment/".repeat(12),
                "日本語のカタログ/skills",
            ] {
                for beside_details in [false, true] {
                    let line = group_label(
                        path,
                        "Agent-specific",
                        "Claude Code + OpenCode",
                        width,
                        beside_details,
                    );
                    assert_eq!(
                        line.width(),
                        usize::from(width),
                        "{width} columns, {path:?}, beside_details {beside_details}: {:?}",
                        label_text(&line)
                    );
                }
            }
        }
    }

    /// Widening the terminal may only ever add. A path readable at one width
    /// that ellipsizes at the next one up is the failure
    /// `viewport::DETAIL_REGION_WIDE_THRESHOLD` exists to prevent for the
    /// inventory table, and a group label is under the same obligation.
    #[test]
    fn widening_the_pane_never_shortens_what_a_group_label_says() {
        for (path, classification, claim) in [
            ("skills", "Common", "all agents"),
            (
                "experimental/claude-code/skills",
                "Agent-specific",
                "Claude Code",
            ),
            (
                "experimental/nested/claude-code/skills",
                "Agent-specific",
                "Claude Code + OpenCode",
            ),
        ] {
            // What the label says of the catalog itself, before the first
            // qualifier: a wider label that spent its extra columns on
            // qualifiers and took them out of the path would say less, not
            // more, so the path is measured on its own as well as the whole.
            // The qualifiers are set flush right, so what the label says of
            // the catalog itself is what precedes the interior gap.
            let named = |label: &str| {
                label
                    .split_once("  ")
                    .map_or_else(|| label.to_owned(), |(path, _)| path.to_owned())
            };
            for beside_details in [false, true] {
                let mut previous = String::new();
                for width in 0..=90_u16 {
                    let current = label_text(&group_label(
                        path,
                        classification,
                        claim,
                        width,
                        beside_details,
                    ));
                    assert!(
                        Span::raw(&current).width() >= Span::raw(&previous).width(),
                        "{path:?} lost content between {} and {width} columns: \
                         {previous:?} then {current:?}",
                        width.saturating_sub(1)
                    );
                    assert!(
                        Span::raw(named(&current)).width() >= Span::raw(named(&previous)).width(),
                        "{path:?} lost path between {} and {width} columns: \
                         {previous:?} then {current:?}",
                        width.saturating_sub(1)
                    );
                    if usize::from(width) >= Span::raw(path).width() {
                        assert_eq!(named(&current), path, "{width} columns");
                    }
                    previous = current;
                }
            }
        }
    }

    /// The qualifiers are set flush against the end of the label, with enough
    /// gap that they read as a separate statement about the catalog rather
    /// than a continuation of its path. That end is the pane's last column
    /// where the pane is the workspace, and the content cap where the detail
    /// region is beside it and the slack past the cap is not the pane's to
    /// keep across the wide crossing.
    #[test]
    fn a_group_labels_qualifiers_are_set_flush_against_the_end_of_the_label() {
        const GAP: usize = 2;

        assert_eq!(GROUP_LABEL_QUALIFIER_GAP, GAP);
        let mut tightest = usize::MAX;

        for width in 0..=120_u16 {
            for beside_details in [false, true] {
                for (path, classification, claim) in [
                    ("skills", "Common", "all agents"),
                    (
                        "experimental/nested/claude-code/skills",
                        "Agent-specific",
                        "Claude Code + OpenCode",
                    ),
                    ("日本語のカタログ/skills", "Common", "all agents"),
                ] {
                    let end = if beside_details {
                        usize::from(width).min(VARIANTS_CONTENT_MAX_WIDTH)
                    } else {
                        usize::from(width)
                    };
                    let line = group_label(path, classification, claim, width, beside_details);
                    let text = label_text(&line);
                    let Some((named, qualifiers)) = text.split_once("  ") else {
                        // No qualifiers survived; the path has the line to
                        // itself.
                        assert!(!text.contains(claim), "{width} columns: {text:?}");
                        continue;
                    };
                    let qualifiers = qualifiers.trim_start();
                    assert!(
                        qualifiers == claim || qualifiers == format!("{classification} · {claim}"),
                        "{width} columns: {text:?}"
                    );
                    assert_eq!(
                        Span::raw(&text).width(),
                        end,
                        "{width} columns, beside_details {beside_details}: \
                         qualifiers not flush against the end: {text:?}"
                    );
                    let gap = end - Span::raw(named).width() - Span::raw(qualifiers).width();
                    assert!(gap >= GAP, "{width} columns: gap of {gap}: {text:?}");
                    // The tightest fit is exactly the gap the label promises,
                    // so the promised gap is pinned and not merely a bound.
                    tightest = tightest.min(gap);
                }
            }
        }
        assert_eq!(tightest, GAP, "no width fitted the qualifiers exactly");
    }

    /// A grouped list of the given shape. `wrapping` gives every row more text
    /// than the window is wide, so the window is exercised in rows rather than
    /// in lines — the variants pane bounds its rows, but the helper is not
    /// entitled to assume that.
    fn grouped(shape: &[usize], wrapping: bool) -> (Vec<Line<'static>>, Vec<usize>) {
        let mut lines = Vec::new();
        let mut group_labels = Vec::new();
        for (group, rows) in shape.iter().enumerate() {
            let label = lines.len();
            lines.push(Line::from(format!("catalog {group}")));
            group_labels.push(label);
            for row in 0..*rows {
                let padding = if wrapping { " and more words" } else { "" };
                lines.push(Line::from(format!("row {group}.{row}{padding}{padding}")));
                group_labels.push(label);
            }
        }
        (lines, group_labels)
    }

    /// Whatever the window does with the label, it must still show the row the
    /// selection is on, must not overrun the region, and must not show one
    /// line twice.
    #[test]
    fn a_grouped_window_always_holds_its_focused_line_once_and_fits_the_region() {
        for shape in [
            vec![1],
            vec![9],
            vec![1, 1],
            vec![0, 4],
            vec![6, 0, 3],
            vec![12, 1],
        ] {
            for wrapping in [false, true] {
                let (lines, group_labels) = grouped(&shape, wrapping);
                for height in 1..=12_usize {
                    for focused in 0..lines.len() {
                        let visible =
                            visible_grouped_lines(&lines, &group_labels, focused, 20, height);
                        let text = visible.iter().map(label_text).collect::<Vec<_>>();
                        let wanted = label_text(&lines[focused]);
                        let where_ = format!(
                            "{shape:?} wrapping={wrapping} at height {height}, \
                             line {focused}: {text:?}"
                        );
                        assert!(text.contains(&wanted), "{where_}");
                        let rows = visible
                            .iter()
                            .map(|line| wrapped_line_count(line, 20))
                            .sum::<usize>();
                        assert!(rows <= height || visible.len() == 1, "{where_}");
                        let mut unique = text.clone();
                        unique.sort();
                        unique.dedup();
                        assert_eq!(unique.len(), text.len(), "{where_}");
                    }
                }
            }
        }
    }

    /// Scrolled deep into a group, the label the rows belong under is pinned
    /// to the top of the window rather than left above it.
    #[test]
    fn a_grouped_window_pins_the_label_of_the_group_it_opens_inside() {
        let (lines, group_labels) = grouped(&[2, 9], false);
        let focused = lines.len() - 1;

        let visible = visible_grouped_lines(&lines, &group_labels, focused, 20, 4);

        let text = visible.iter().map(label_text).collect::<Vec<_>>();
        assert_eq!(text[0], "catalog 1", "{text:?}");
        assert!(text.contains(&"row 1.8".to_owned()), "{text:?}");
    }

    /// A stored scan time is seconds since the epoch, which says nothing to a
    /// reader. Every case here was checked against `date -u`.
    #[test]
    fn a_scan_timestamp_reads_as_a_civil_date_in_utc() {
        assert_eq!(format_scan_timestamp(1_785_903_291), "2026-08-05 04:14 UTC");
        assert_eq!(format_scan_timestamp(0), "1970-01-01 00:00 UTC");
        // The far end of a plausible stored value, and the last minute of a
        // year, so a day-of-year that rolls over is caught.
        assert_eq!(format_scan_timestamp(4_102_444_799), "2099-12-31 23:59 UTC");
        // A leap day in a century year that is a leap year.
        assert_eq!(format_scan_timestamp(951_825_600), "2000-02-29 12:00 UTC");
    }

    /// A timestamp before the epoch is not something Skilled stores, but the
    /// formatter is given an `i64` and must answer rather than panic or wrap.
    #[test]
    fn a_scan_timestamp_before_the_epoch_still_reads_as_a_date() {
        assert_eq!(format_scan_timestamp(-1), "1969-12-31 23:59 UTC");
        // The extremes of the type answer as well, rather than overflowing.
        for seconds in [i64::MIN, i64::MAX] {
            assert!(
                format_scan_timestamp(seconds).ends_with(" UTC"),
                "{seconds} should still format"
            );
        }
    }

    /// A middle-truncated value falls back to the ellipsis itself when there is
    /// no room for anything either side of it, and a `Path` field showing `.`
    /// or `..` would be stating a path rather than eliding one. The narrowest
    /// detail region leaves every field far more than that, and this pins it:
    /// the fallback stays unreachable however the region is laid out.
    #[test]
    fn every_middle_truncated_detail_field_has_room_for_both_ends_of_its_value() {
        const ELLIPSIS_ONLY: usize = 3;
        for label in ["Path", "Remote"] {
            let budget = detail_value_budget(label, NARROWEST_INNER_WIDTH);
            assert!(
                budget > ELLIPSIS_ONLY,
                "{label:?} has only {budget} cells in the narrowest detail region"
            );
            let elided = terminal_safe_bounded_middle(&"a/".repeat(64), budget);
            assert!(
                elided.starts_with('a') && elided.ends_with('/') && elided.contains("..."),
                "{elided:?} should keep both ends of the value it stands for"
            );
        }
    }

    /// The claim names agents, so a claim cut short names a different one:
    /// `Claude Code + Open...` is not what was stored, and unlike a path it
    /// carries no sense of the value it stands for. The longest two agents can
    /// make of it fills the narrowest region's line to the cell, so every
    /// combination is checked rather than the one a fixture happens to hold.
    #[test]
    fn every_compatibility_claim_stands_whole_in_the_narrowest_detail_region() {
        for claude_code in [false, true] {
            for codex in [false, true] {
                for opencode in [false, true] {
                    let claim = compatibility_claim(Compatibility::from_flags(
                        claude_code,
                        codex,
                        opencode,
                    ));
                    let stated = label_text(&detail_field_bounded(
                        "Compatibility",
                        &claim,
                        NARROWEST_INNER_WIDTH,
                        1,
                    ));
                    assert_eq!(
                        stated,
                        format!("Compatibility: {claim}"),
                        "the claim should be stated whole"
                    );
                }
            }
        }
    }
}
