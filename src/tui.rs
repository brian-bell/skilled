use std::path::Path;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Margin, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap},
};

use crate::{
    AgentKind, DoctorItem, DoctorPane, InventoryPane, RegistryAvailability, SessionIdentity,
    SetupStep, SkilledApp, SourcesPane, UpdatesPane, View,
    app::{MAX_INVENTORY_FILTER, SourceRow, catalog_rows},
    components::{self, KeyHint, terminal_safe},
    inventory::{
        Finding, FindingSeverity, InstallationHealth, InstallationObject,
        InstalledSkillObservation, InventoryRow, RootScan, RootStatus, RowProvenance, RowVerdict,
    },
    operations::{
        AppliedStep, ExcludedReason, ForgetApply, ForgetPrompt, ForgetReceiptState, ForgetStatus,
        ForgetVerification, InstallOutcome, InstallPlan, InstallPrompt, InstallStatus,
        InstallTarget, OperationPrompt, RepairDisposition, RepairOfferStatus, RepairOutcome,
        RepairPlan, RepairPrompt, RepairStatus, RepairStepOutcome, StepOutcome, TargetDisposition,
        UninstallDisposition, UninstallPrompt, UninstallStatus,
    },
    resolution::{OpenCodeEntry, OpenCodeResolution, UnknownCause},
    source::{
        CatalogClassification, CatalogProposal, Compatibility, RegisteredSource, SkillCandidate,
        SkillValidation,
    },
    theme::{self, Tone},
    updates::{RepositoryUpdatePrompt, RepositoryUpdateVerdict},
    viewport,
};

pub const MINIMUM_WIDTH: u16 = 80;
pub const MINIMUM_HEIGHT: u16 = 24;

/// What a frame measured that the application state has no way to know.
///
/// The reducer is deliberately geometry-blind — `update` never learns the
/// terminal's size — yet a scrollable region has to be clamped to content the
/// renderer alone can measure. The frame reports what it found and the runner
/// notes it, the same boundary a typed [`crate::Effect`] crosses for
/// filesystem work.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderFeedback {
    detail_max_scroll: Option<usize>,
    update_preview_fully_seen: Option<bool>,
}

impl RenderFeedback {
    /// The furthest the Inventory detail region could be scrolled and still
    /// show rows that were not already visible: zero where it holds everything
    /// it has, and `None` where this frame did not draw it at all and so
    /// measured nothing.
    pub fn detail_max_scroll(self) -> Option<usize> {
        self.detail_max_scroll
    }

    pub fn update_preview_fully_seen(self) -> Option<bool> {
        self.update_preview_fully_seen
    }
}

pub fn render(frame: &mut Frame<'_>, app: &SkilledApp) -> RenderFeedback {
    let area = frame.area();
    if area.width < MINIMUM_WIDTH || area.height < MINIMUM_HEIGHT {
        render_size_notice(frame, area);
        return RenderFeedback::default();
    }

    // The prototype does not run its terminal to the viewport's edge: the
    // page shows around a bordered window (`.terminal`,
    // spec/tui-prototype.html:55-67), and this ring is that edge — the
    // window line drawn through one cell of page ground. It appears only
    // where its two columns and two rows come out of surplus: the 80×24
    // minimum is a content guarantee, and a frame that taxed it would shrink
    // the workspace the minimum exists to protect. Everything below works in
    // the inset area, so no surface, dialog, or overlay can reach the ring —
    // it carries strokes, never text (see `theme::app_frame`).
    let area = if area.width >= MINIMUM_WIDTH + 2 && area.height >= MINIMUM_HEIGHT + 2 {
        frame.render_widget(app_frame_ring(area), area);
        area.inner(Margin::new(1, 1))
    } else {
        area
    };
    frame.render_widget(Block::new().style(theme::app_surface()), area);

    // The chrome takes the prototype's own bar heights where the terminal is
    // tall enough to afford them, and a single row each where it is not; the
    // three bars share one threshold so they cannot disagree about which
    // terminal is tall.
    let [title_bar, navigation, workspace, key_hints] = Layout::vertical([
        Constraint::Length(viewport::title_bar_height(area.height)),
        Constraint::Length(viewport::nav_bar_height(area.height)),
        Constraint::Min(1),
        Constraint::Length(viewport::chrome_bar_height(area.height)),
    ])
    .areas(area);

    // One decision for both chrome rows, so they cannot disagree about which
    // of them carries the session status.
    let status_on_nav = session_status_on_nav_row(app, area);
    render_title_bar(frame, title_bar, app, status_on_nav);
    render_navigation(frame, navigation, app, status_on_nav);
    let body = workspace;
    // Measured once, for this frame's geometry, and used by everything that
    // speaks about the detail region: the window drawn, the key hint, and the
    // help entry then cannot disagree with one another or lag a keystroke
    // behind the terminal they are describing.
    // Ordered once, for this frame: the findings list, the detail region beside
    // it, and the extent measured for that region must all rest on the same
    // order, and building it three times would pay for the sort three times to
    // prove it. Every other view leaves it empty and pays nothing.
    let findings = match app.view() {
        View::Doctor => app.doctor_findings(),
        _ => Vec::new(),
    };
    let detail_extent = detail_scroll_extent(app, area, workspace, &findings);
    let update_preview_seen = update_preview_fully_seen(app, area);
    // The grid's rules answer to the same height the chrome bars measure, so
    // the workspace and the bars agree about which terminal is tall.
    let airy = viewport::airy_rows(area.height);
    match app.view() {
        View::Setup(step) => render_setup(frame, body, app, step),
        View::Inventory => render_inventory(frame, body, app, airy),
        View::Sources => render_sources(frame, body, app),
        View::Updates => render_updates(frame, body, app),
        View::Doctor => render_doctor(frame, body, app, &findings),
        View::Settings => {
            render_inventory(frame, body, app, airy);
            render_settings(frame, body, app);
        }
    }
    if let Some(prompt) = app.pending_operation() {
        match prompt {
            OperationPrompt::Install(prompt) => render_install_prompt(
                frame,
                area,
                prompt,
                app.home(),
                app.detail_scroll(),
                detail_extent,
                operation_preview_fully_seen(app, detail_extent),
            ),
            OperationPrompt::Uninstall(prompt) => render_uninstall_prompt(
                frame,
                area,
                prompt,
                app.detail_scroll(),
                detail_extent,
                operation_preview_fully_seen(app, detail_extent),
            ),
            OperationPrompt::Forget(prompt) => render_forget_prompt(
                frame,
                area,
                prompt,
                app.detail_scroll(),
                detail_extent,
                operation_preview_fully_seen(app, detail_extent),
            ),
        }
    } else if let Some(prompt) = app.pending_repair() {
        render_repair_prompt(
            frame,
            area,
            prompt,
            app.detail_scroll(),
            detail_extent,
            preview_fully_seen(app, detail_extent),
        );
    } else if let Some(prompt) = app.pending_update() {
        render_update_prompt(
            frame,
            area,
            prompt,
            app.detail_scroll(),
            detail_extent,
            app.update_preview_fully_seen() || update_preview_seen == Some(true),
        );
    } else if app.source_path_input_active() {
        render_source_path_entry(frame, area, app);
    } else if app.pending_source().is_some() && app.view() == View::Sources {
        render_catalog_confirmation(frame, area, app);
    }
    if let Some(context) = app.help_context() {
        render_help(frame, area, context, app, &findings, detail_extent);
    }
    render_footer(
        frame,
        key_hints,
        app,
        &findings,
        detail_extent,
        update_preview_seen,
    );
    RenderFeedback {
        detail_max_scroll: detail_extent,
        update_preview_fully_seen: update_preview_seen,
    }
}

/// The window frame's ring, ready to render across the whole terminal.
///
/// Not a bordered [`Block`]: box-drawing lines cross the middle of their
/// cells, which leaves half a cell of page ground *inside* the line, between
/// the border and the title band it should sit against. Each stroke here is
/// an eighth-block hugging the inner edge of its frame cell — `▁` along the
/// bottom of the top row, `▔` along the top of the bottom row, `▕` and `▏`
/// against the content on either side — so the line lands flush on the
/// surfaces it frames and the page shows only outside the rectangle, which
/// is where the prototype keeps it. The corner cells stay bare ground: the
/// strokes' edges already meet at the corner point, and a corner glyph would
/// have to be a quarter-block several times the strokes' weight.
fn app_frame_ring(area: Rect) -> Paragraph<'static> {
    let span = usize::from(area.width).saturating_sub(2);
    let horizontal = |stroke: &str| Line::raw(format!(" {} ", stroke.repeat(span)));
    let side = format!("▕{}▏", " ".repeat(span));
    let mut rows = Vec::with_capacity(usize::from(area.height));
    rows.push(horizontal("▁"));
    for _ in 2..area.height {
        rows.push(Line::raw(side.clone()));
    }
    rows.push(horizontal("▔"));
    Paragraph::new(rows).style(theme::app_frame())
}

/// The persistent title row: product mark, wordmark, context path, and —
/// when the navigation row cannot hold it — the session status.
///
/// Recorded departure: the prototype's titlebar ends in an `interactive
/// prototype · no filesystem writes` flag (spec/tui-prototype.html:531).
/// The flag is prototype-only, and its claim would be false for the real
/// application — installation writes — so no equivalent exists here.
fn render_title_bar(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp, status_on_nav: bool) {
    // The band goes down first and the paragraphs only carry foreground
    // colours, so it survives underneath them.
    frame.render_widget(Block::new().style(theme::chrome_band()), area);
    // The band's last row carries the prototype's titlebar border
    // (`.terminal-titlebar` `border-bottom`): a hairline hugging the pad
    // row's bottom edge, flush against the tab strip below. A single-row
    // band has no pad row to carry it and stays unruled.
    if area.height > 1 {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "▁".repeat(usize::from(area.width)),
                theme::title_rule(),
            )),
            Rect {
                y: area.y + area.height - 1,
                height: 1,
                ..area
            },
        );
    }
    // On a tall terminal the band is the prototype's bar with its text
    // centred: three rows, pad–text–pad, per `viewport::title_bar_height`.
    let area = components::centered_band_row(area);

    let product = if status_on_nav {
        area
    } else {
        // The status gets its own rectangle because a Paragraph repaints its
        // whole area before drawing: rendering it across the full row would
        // silently flatten the product mark and wordmark to the status colour.
        let [product, session] = Layout::horizontal([
            Constraint::Min(0),
            Constraint::Length(session_status_width(app).min(area.width)),
        ])
        .areas(area);
        render_session_status(frame, session, app);
        product
    };

    let prefix = [
        Span::styled(" ◆ ", theme::product_mark()),
        Span::styled("skilled", theme::product_name()),
    ];
    // Measured from the spans themselves so a reworded mark or wordmark
    // cannot silently mis-budget the truncation; plus the two-column gap the
    // prototype leaves before the path, and one more column so the path can
    // never end flush against the status glyph beside it.
    let reserved = prefix.iter().map(Span::width).sum::<usize>() + 3;
    let context = context_path(
        app.identity(),
        usize::from(product.width).saturating_sub(reserved),
    );
    let mut spans = prefix.to_vec();
    spans.push(Span::styled(format!("  {context}"), theme::chrome()));
    frame.render_widget(Paragraph::new(Line::from(spans)), product);
}

/// Whether this frame puts the session status beside the navigation, which
/// is where the prototype places it.
///
/// Only at [`viewport::Viewport::Wide`], and only when the whole status fits
/// beside the whole row: a strip clipped by the status could cut a count's
/// trailing digit and turn `·12` into a smaller claim, and a clipped status
/// would misreport the session, so whichever cannot fit whole sends the
/// status back to the title bar instead.
///
/// An exact fit is allowed — unlike the title bar, which reserves a seam
/// column before the status — because the tab strip ends in its own `│`
/// separator, a boundary glyph already in place. A keyboard owner's note has
/// no such closing glyph and may touch the status dot at an exact fit; the
/// dot's colour break is accepted as the seam there.
///
/// While an overlay dialog floats, the status returns to the title bar even
/// where it would fit: a dialog clears only its own popup, so a status left
/// on the navigation row would be cut at the dialog's border and its tail
/// would hang past the frame as a stray fragment. The title bar is the row
/// the popups leave alone.
fn session_status_on_nav_row(app: &SkilledApp, area: Rect) -> bool {
    !overlay_open(app)
        && viewport::classify(area) == viewport::Viewport::Wide
        && navigation_row_line(app, viewport::nav_bar_height(area.height) > 1).width()
            + usize::from(session_status_width(app))
            <= usize::from(area.width)
}

/// Whether this frame floats a dialog over the shell — the same conditions,
/// in the same precedence, that [`render`] draws overlays under, plus the
/// help modal that can sit over any of them.
fn overlay_open(app: &SkilledApp) -> bool {
    app.pending_operation().is_some()
        || app.pending_repair().is_some()
        || app.source_path_input_active()
        || (app.pending_source().is_some() && app.view() == View::Sources)
        || app.help_context().is_some()
}

/// The columns the session status asks of whichever row carries it: its
/// glyph, its label, and a one-column margin against the terminal's edge.
fn session_status_width(app: &SkilledApp) -> u16 {
    let status = SessionStatus::of(app);
    u16::try_from(Span::raw(status.label()).width() + 3).unwrap_or(u16::MAX)
}

/// The session status, right-aligned in its region on whichever chrome row
/// the viewport assigned it. Only a foreground: the region keeps the band or
/// navigation surface already painted beneath it.
fn render_session_status(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp) {
    let status = SessionStatus::of(app);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("● ", theme::tone_style(status.tone())),
            Span::styled(status.label(), theme::chrome()),
            Span::raw(" "),
        ]))
        .alignment(Alignment::Right),
        area,
    );
}

/// The context path beside the wordmark (prototype `.terminal-path`):
/// `global · user@host · macOS`, with an absent segment and its separator
/// omitted rather than invented, and the user and host escaped through
/// [`terminal_safe`] because both come from outside Skilled.
///
/// A path too wide for its half of the row sheds segments whole — host first,
/// then user, then the operating system — rather than colliding with the
/// status beside it: `global · brian · macOS` still identifies the session
/// where `global · brian@mac…` would identify nothing. The scope word always
/// remains; at worst the layout clips it.
fn context_path(identity: &SessionIdentity, width: usize) -> String {
    let user = identity.user.as_deref().map(terminal_safe);
    let host = identity.host.as_deref().map(terminal_safe);
    let os = identity.os.as_deref().map(terminal_safe);
    let session = match (&user, &host) {
        (Some(user), Some(host)) => Some(format!("{user}@{host}")),
        (Some(user), None) => Some(user.clone()),
        (None, Some(host)) => Some(host.clone()),
        (None, None) => None,
    };

    let shedding = [
        [session.as_deref(), os.as_deref()],
        [user.as_deref(), os.as_deref()],
        [None, os.as_deref()],
    ];
    for step in shedding {
        let path = std::iter::once("global")
            .chain(step.into_iter().flatten())
            .collect::<Vec<_>>()
            .join(" · ");
        if Span::raw(&path).width() <= width {
            return path;
        }
    }
    "global".to_owned()
}

/// What the application can honestly say about the current session.
///
/// Skilled performs no network access in this release, so the status may only
/// describe setup progress and what the local scan observed.
///
/// Recorded departure: the prototype's session label reads `scan complete ·
/// 2s ago` (spec/tui-prototype.html:541). The reducer is time-blind and the
/// redraw event-driven, so a relative timestamp would sit on screen going
/// stale. A future `scanning…` state may rest on
/// [`crate::inventory::InventorySnapshot::scan_pending`]; a clock claim,
/// never.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionStatus {
    Degraded,
    SetupInProgress,
    Ready { sources: usize },
}

impl SessionStatus {
    fn of(app: &SkilledApp) -> Self {
        if app.metadata_failure().is_some() {
            return Self::Degraded;
        }
        match app.view() {
            View::Setup(_) => Self::SetupInProgress,
            _ => Self::Ready {
                sources: app.sources().len(),
            },
        }
    }

    fn tone(self) -> Tone {
        match self {
            Self::Degraded => Tone::Critical,
            Self::SetupInProgress => Tone::Warning,
            Self::Ready { .. } => Tone::Healthy,
        }
    }

    fn label(self) -> String {
        match self {
            Self::Degraded => "degraded · metadata unavailable".to_owned(),
            Self::SetupInProgress => "setup in progress".to_owned(),
            Self::Ready { sources: 1 } => "ready · 1 source registered".to_owned(),
            Self::Ready { sources } => format!("ready · {sources} sources registered"),
        }
    }
}

fn render_navigation(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp, status_on_nav: bool) {
    frame.render_widget(Block::new().style(theme::nav_surface()), area);
    let tall = area.height > 1;

    // When the status shares this row — the prototype's placement — it lives
    // beside whatever the row is showing, a tab strip or a keyboard owner:
    // the status is the one part of the chrome that keeps reporting while a
    // dialog holds the keys, so it does not vanish with the tabs.
    // `session_status_on_nav_row` has already measured that both fit whole.
    let strip = if status_on_nav {
        let [strip, session] = Layout::horizontal([
            Constraint::Min(0),
            Constraint::Length(session_status_width(app).min(area.width)),
        ])
        .areas(area);
        render_session_status(frame, components::centered_band_row(session), app);
        strip
    } else {
        area
    };
    // The labels keep the strip's middle row, centred between its padding
    // the way the prototype's `.tab` padding centres them.
    frame.render_widget(
        Paragraph::new(navigation_row_line(app, tall)),
        components::centered_band_row(strip),
    );
    if tall {
        // The row above the labels: each cell's surface and separator carried
        // up, so a tab's box spans the strip's full height.
        frame.render_widget(
            Paragraph::new(navigation_padding_line(app)),
            Rect { height: 1, ..strip },
        );
        // The strip's last row is the prototype's `.app-nav` bottom border,
        // drawn across the whole row — under the cells, the slack, and the
        // session status alike — with the active tab's stretch in the accent
        // colour. The keyboard owner's takeover has no tabs, so its row
        // keeps the plain hairline and no tab claims a stretch of it.
        frame.render_widget(
            Paragraph::new(navigation_accent_line(app, area.width)),
            Rect {
                y: area.y + area.height - 1,
                height: 1,
                ..area
            },
        );
    }
}

/// One destination's cell of the tab strip, and whether it is the active one.
///
/// The accent row must end each border exactly where the label row ends its
/// tab, so both rows are laid out from the same cells.
fn navigation_cells(app: &SkilledApp, tall: bool) -> Vec<(Vec<Span<'static>>, bool)> {
    let mut cells = Vec::new();
    for destination in Destination::ALL {
        let active = destination.is_active(app.view());
        let style = match (destination.is_available(), active, tall) {
            (false, _, _) => theme::nav_disabled(),
            (true, true, false) => theme::nav_active(),
            (true, true, true) => theme::nav_active_tall(),
            (true, false, _) => theme::nav_inactive(),
        };
        // Every cell leads with the same padding space — the strip does not
        // borrow the list-focus marker. The active entry is already said
        // three ways: the raised surface, the bold label, and the
        // accent-coloured stretch of the bottom border (the underline, on a
        // single-row strip).
        let mut cell = vec![Span::styled(" ", style)];
        // The digit is the prototype's `.tab-key`, part of every available
        // tab's caption — the active tab's included, by decision: hiding it
        // there made the active tab the one cell without its number, which
        // read as a different kind of entry rather than the same tab
        // selected. On the active view the digit is inert, and that is what
        // pressing the number of the tab already on screen should be; it is
        // caption, not a key hint. A destination this release cannot open
        // still shows no digit, because there it would advertise a route
        // that does not exist.
        let key = if destination.is_available() {
            format!("{} ", destination.key())
        } else {
            String::new()
        };
        cell.push(Span::styled(key, style.patch(theme::nav_key())));
        cell.push(Span::styled(
            format!(
                "{}{} ",
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
            cell.push(Span::styled(
                format!("·{count} "),
                style.patch(theme::nav_count()),
            ));
        }
        // The pad carries the cell's own style, so an active cell's raised
        // surface and underline reach its separator instead of stopping at
        // the last glyph and leaving the padding on the bare band.
        let content: usize = cell.iter().map(Span::width).sum();
        if content < NAV_CELL_MIN_WIDTH {
            cell.push(Span::styled(
                " ".repeat(NAV_CELL_MIN_WIDTH - content),
                style,
            ));
        }
        cells.push((cell, active));
    }
    cells
}

/// The navigation row's content: the keyboard owner's takeover, or the boxed
/// tab strip. Built apart from [`render_navigation`] so the placement
/// decision can measure the same line the frame will draw. `tall` says the
/// strip has an accent row beneath it, which changes the active entry's style
/// and nothing about the line's width.
fn navigation_row_line(app: &SkilledApp, tall: bool) -> Line<'static> {
    if let Some((owner, note)) = keyboard_owner(app) {
        return Line::from(vec![
            Span::styled(format!(" {owner} "), theme::nav_active()),
            Span::styled(format!("  {note}"), theme::nav_note()),
        ]);
    }

    let mut spans = Vec::new();
    for (mut cell, _) in navigation_cells(app, tall) {
        spans.append(&mut cell);
        spans.push(Span::styled("│", theme::nav_separator()));
    }
    Line::from(spans)
}

/// A padding row of the tall strip: each cell's surface and separator
/// carried through the row above the labels, so a tab's box — its raised
/// active surface and the side borders closing it — spans the strip's full
/// height rather than stopping at the label (prototype `.tab` padding). The
/// keyboard owner's takeover has no cells, so its padding row is bare strip.
fn navigation_padding_line(app: &SkilledApp) -> Line<'static> {
    let mut spans = Vec::new();
    if keyboard_owner(app).is_none() {
        for (cell, active) in navigation_cells(app, true) {
            let cell_width: usize = cell.iter().map(Span::width).sum();
            let surface = if active {
                theme::nav_active_tall()
            } else {
                theme::nav_inactive()
            };
            spans.push(Span::styled(" ".repeat(cell_width), surface));
            spans.push(Span::styled("│", theme::nav_separator()));
        }
    }
    Line::from(spans)
}

/// The tall strip's last row: the prototype's `.app-nav` bottom border, with
/// the active tab's stretch in the accent colour
/// (`.tab[aria-selected="true"]` `border-bottom-color`), under exactly the
/// cell the label row gives that tab. Each separator column keeps its `│`,
/// running the tab's side border down to meet the line at the strip's edge,
/// and carries the border through its own column as an underline (see
/// `theme::nav_separator_at_rule`); past the last cell the hairline runs
/// unbroken to the row's end — under the slack and the session state — so
/// together with the titlebar's rule above, every cell reads as a closed
/// box.
fn navigation_accent_line(app: &SkilledApp, width: u16) -> Line<'static> {
    let mut spans = Vec::new();
    if keyboard_owner(app).is_none() {
        for (cell, active) in navigation_cells(app, true) {
            let cell_width: usize = cell.iter().map(Span::width).sum();
            spans.push(if active {
                Span::styled("▁".repeat(cell_width), theme::nav_accent())
            } else {
                Span::styled("▁".repeat(cell_width), theme::nav_rule())
            });
            spans.push(Span::styled("│", theme::nav_separator_at_rule()));
        }
    }
    // On to the row's end; the Paragraph clips the surplus.
    spans.push(Span::styled(
        "▁".repeat(usize::from(width)),
        theme::nav_rule(),
    ));
    Line::from(spans)
}

/// The narrowest navigation cell: the prototype's tabs reserve
/// `min-width: 126px`, sixteen columns at the same ~8px/cell conversion
/// [`viewport`] uses for the aside. A count can push a cell wider; the
/// minimum keeps the strip's rhythm when it does not.
const NAV_CELL_MIN_WIDTH: usize = 16;

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
    if app.pending_update().is_some() {
        return Some(("Repository update".to_owned(), DIALOG_NOTE));
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
        View::Inventory | View::Sources | View::Updates | View::Doctor => None,
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
        true
    }

    /// What this destination can honestly say it holds, if anything.
    ///
    /// Sources has a count only when its registry was read. The inventory is
    /// an observation of the filesystem, and
    /// whether that observation may be stated as a number is decided by
    /// [`crate::inventory::InventorySnapshot::stated_skill_count`] — the same decision
    /// [`inventory_subtitle`] defers to, so the tab and the subtitle beneath it
    /// cannot disagree. A destination this release cannot open has nothing to
    /// count and renders nothing: an em dash would read as a measurement that
    /// came back empty.
    fn count(self, app: &SkilledApp) -> Option<usize> {
        match self {
            Self::Inventory => app.inventory().stated_skill_count(),
            Self::Sources => (app.registry_availability() == RegistryAvailability::Readable)
                .then(|| app.sources().len()),
            // Findings are observations of the same roots the inventory reads,
            // so the same verdict decides whether either may be stated.
            Self::Doctor => app.stated_finding_count(),
            Self::Updates => app.stated_update_count(),
        }
    }

    fn is_active(self, view: View) -> bool {
        match self {
            Self::Inventory => matches!(view, View::Inventory | View::Settings),
            Self::Sources => view == View::Sources,
            Self::Doctor => view == View::Doctor,
            Self::Updates => view == View::Updates,
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
fn render_inventory(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp, airy: bool) {
    match viewport::workspace_regions(area) {
        (primary, Some(detail)) => {
            render_inventory_skills(frame, primary, app, airy);
            render_inventory_detail(frame, detail, app, true);
        }
        (primary, None) => match app.inventory_pane() {
            InventoryPane::Skills => render_inventory_skills(frame, primary, app, airy),
            InventoryPane::Details => render_inventory_detail(frame, primary, app, false),
        },
    }
}

/// Column widths for the installation table.
///
/// The three agent columns and the health column are sized by their headings,
/// which never change; the identity columns divide whatever is left, up to a
/// cap. On a table wide enough that both identity caps still bind, the agent
/// columns widen instead to carry each observation's health label.
#[derive(Clone, Copy)]
struct InventoryColumns {
    skill: usize,
    source: usize,
    /// Whether the agent columns carry a health label beside each glyph.
    labeled: bool,
    /// Whether the columns are separated by the prototype's grid rules
    /// (`.grid-head > span:not(:first-child)`, spec/tui-prototype.html:243).
    chrome: bool,
}

impl InventoryColumns {
    fn agent_widths(self) -> [usize; 3] {
        if self.labeled {
            LABELED_AGENT_COLUMN_WIDTHS
        } else {
            AGENT_COLUMN_WIDTHS
        }
    }

    /// The columns the grid's vertical rules stand in, measured from the
    /// row's left edge: after the marker, each cell's width and then the
    /// rule with its clearance. A horizontal rule crossing these columns
    /// takes a junction there, so the vertical reads as one line through the
    /// grid's whole height. Empty when the chrome is collapsed.
    fn rule_offsets(self) -> Vec<usize> {
        if !self.chrome {
            return Vec::new();
        }
        let mut offsets = Vec::with_capacity(INVENTORY_COLUMN_COUNT - 1);
        let mut x = ROW_MARKER_WIDTH;
        for width in [self.skill, self.source]
            .into_iter()
            .chain(self.agent_widths())
        {
            x += width;
            offsets.push(x);
            x += 2;
        }
        offsets
    }
}

const AGENT_COLUMN_WIDTHS: [usize; 3] = [8, 7, 10];
/// The agent columns when they carry labels: the longest labelled cell —
/// `- not a skill`, thirteen cells — plus the same column of clearance the
/// Health column keeps.
///
/// The prototype's `.agent-state` cells pair every glyph with the state's
/// word and collapse to the glyph alone below its 1050px breakpoint
/// (`.agent-state span { display: none }`). The labels here claim only the
/// slack the capped identity columns leave behind — see
/// [`inventory_columns`] — so no name or source narrows to pay for a label,
/// and the columns still match on either side of the workspace's
/// wide-detail crossing ([`viewport::DETAIL_REGION_WIDE_THRESHOLD`]).
const LABELED_AGENT_COLUMN_WIDTHS: [usize; 3] = [14, 14, 14];
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

/// What the grid's rules cost a row: one rule cell and one cell of clearance
/// after it, at each of the five boundaries between the six columns. The
/// clearance *before* each rule is the trailing space every [`padded`] column
/// already guarantees, so the boundary reads ` │ ` without charging for
/// three cells.
///
/// The chrome enters only above both identity caps (skilled-hjo): below
/// that, every column is exactly what it was before the chrome existed, so
/// the rules can never take width from a name or cost the Source column its
/// place — today's columns are the floor, and the rules come out of slack.
const GRID_CHROME_WIDTH: usize = (INVENTORY_COLUMN_COUNT - 1) * 2;
/// Skill, Source, the three agents, and Health.
const INVENTORY_COLUMN_COUNT: usize = 6;

fn inventory_columns(width: u16) -> InventoryColumns {
    // Labels enter only after both identity caps and the grid chrome are
    // fully served: below that, every column is exactly what it was before
    // labels existed, so widening a terminal can never take width from a
    // name to spend on a word the glyph already implies. The chrome sits
    // below the labels in the progression — the prototype draws its rules at
    // every width and its labels only past a breakpoint
    // (`.agent-state span`, spec/tui-prototype.html:500), so as a terminal
    // widens the rules arrive first, and the label threshold moves out by
    // the chrome's cost rather than the labels ever appearing unruled.
    let labeled_fixed =
        ROW_MARKER_WIDTH + LABELED_AGENT_COLUMN_WIDTHS.iter().sum::<usize>() + HEALTH_COLUMN_WIDTH;
    if usize::from(width) >= labeled_fixed + MAX_SKILL_WIDTH + MAX_SOURCE_WIDTH + GRID_CHROME_WIDTH
    {
        return InventoryColumns {
            skill: MAX_SKILL_WIDTH,
            source: MAX_SOURCE_WIDTH,
            labeled: true,
            chrome: true,
        };
    }
    let fixed = ROW_MARKER_WIDTH + AGENT_COLUMN_WIDTHS.iter().sum::<usize>() + HEALTH_COLUMN_WIDTH;
    if usize::from(width) >= fixed + MAX_SKILL_WIDTH + MAX_SOURCE_WIDTH + GRID_CHROME_WIDTH {
        return InventoryColumns {
            skill: MAX_SKILL_WIDTH,
            source: MAX_SOURCE_WIDTH,
            labeled: false,
            chrome: true,
        };
    }
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
            labeled: false,
            chrome: false,
        };
    }
    InventoryColumns {
        skill,
        source,
        labeled: false,
        chrome: false,
    }
}

fn render_inventory_skills(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp, airy: bool) {
    let rows = app.filtered_rows();
    // The prototype's pane header keeps clearance above its content as well
    // as beneath it (`.pane-header`, spec/tui-prototype.html:167: `min-height:
    // 48px`, `padding: 8px 12px`). The row above lives inside the pane rather
    // than being cut from the workspace, so the rule that splits off the
    // Details pane runs through it and meets the bar above.
    let mut header_lines = vec![
        Line::default(),
        pane_header(
            "Global inventory",
            &qualified_inventory_subtitle(app, rows.len()),
            app.inventory_pane() == InventoryPane::Skills,
            area.width,
        ),
    ];
    if app.inventory_filter_active() || !app.inventory_filter().is_empty() {
        header_lines.push(inventory_filter_line(app));
    }
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
    if let Some(line) = metadata_failure_line(app, area.width) {
        header_lines.push(line);
    }

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
    let columns = inventory_columns(body.width);
    let rule_offsets = columns.rule_offsets();
    // The block closes at its bottom edge, like the Details pane beside it
    // (see `PADDED_PANE_HEADER_HEIGHT` for why the border is not a centred
    // rule) — except where a tall terminal sets the ruled grid directly
    // beneath. There the closing border is the grid's own top rule: centred
    // ink like every rule below it, so the headings keep the same half-row
    // of air above them that they and every row keep below, and junctioned
    // `┬` where the column verticals begin. One line serves as both borders,
    // the way the prototype's pane-header border sits directly over its
    // `.grid-head`. The Details pane beside it still closes with `▁`: it
    // closes a pane, not a grid, and the two idioms may differ across the
    // region separator.
    if airy && !rows.is_empty() {
        header_lines.push(components::grid_rule_row(header.width, &rule_offsets, '┬'));
    } else {
        header_lines.push(components::underline(header.width));
    }
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

    let mut lines = vec![inventory_column_headings(columns)];
    // The heading's closing rule and each row's bottom rule spend whole rows,
    // so they draw only on the tall terminal the gate names; a short one
    // keeps every row for an entry. The last visible row's rule may fall
    // past the pane's bottom edge, where the widget clips it — the rows a
    // capacity of entries promises are never the lines given up to air.
    //
    // The rules are centred `─` ink, not the pane header's bottom-edge `▁`:
    // a rule row's spare space falls to whichever side of its ink, and only
    // the centred glyph splits it evenly, so the text between two rules sits
    // vertically centred the way the prototype centres a row's content
    // (`.grid-head`/`.data-row`, `align-items: center`,
    // spec/tui-prototype.html:225). Each rule junctions the column verticals
    // through itself — `┼` while further rows follow, `┴` under the last row
    // of the table, which is the table's whole extent and not the window's:
    // a rule that closed the columns at the window's edge would claim an end
    // the data does not have.
    if airy {
        lines.push(components::grid_rule_row(body.width, &rule_offsets, '┼'));
    }
    let available = usize::from(body.height.max(1)).saturating_sub(lines.len());
    let capacity = if airy {
        available.div_ceil(2)
    } else {
        available
    };
    let start = visible_window_start(app.focused_installation(), capacity);
    for (index, row) in rows.iter().enumerate().skip(start).take(capacity) {
        lines.push(inventory_row_line(
            row,
            columns,
            index == app.focused_installation(),
            body.width,
        ));
        if airy {
            let junction = if index + 1 == rows.len() {
                '┴'
            } else {
                '┼'
            };
            lines.push(components::grid_rule_row(
                body.width,
                &rule_offsets,
                junction,
            ));
        }
    }
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
    if app.metadata_failure().is_some() && inventory.stated_skill_count() == Some(0) {
        return "nothing installed · metadata unavailable".to_owned();
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

/// The claim a completed scan makes, qualified by the roots it was never
/// asked about.
///
/// A stated count is complete over the roots the user selected, so a count
/// can stand while an agent's column shows only `-`. Without a qualifier
/// that `-` under a deselected agent would read as a root that was read and
/// found empty — the flattening the inventory's truthfulness rule forbids.
/// The qualifier is the subtitle's last clause, so a narrow pane sheds it
/// after the count it qualifies ([`bounded_subtitle`]). The pending and
/// all-deselected states keep their single phrases: "no root read" speaks
/// for every root at once, and "not scanned" stands while nothing per-agent
/// is on screen yet — a deselected root beside pending ones becomes worth
/// naming only when a count starts speaking for the roots that were read.
fn qualified_inventory_subtitle(app: &SkilledApp, shown: usize) -> String {
    let claim = inventory_subtitle(app, shown);
    let inventory = app.inventory();
    if inventory.scan_pending() || inventory.no_agent_configured() {
        return claim;
    }
    let deselected: Vec<&str> = inventory
        .roots()
        .iter()
        .filter(|root| matches!(root.status(), RootStatus::NotSelected))
        .map(|root| root.agent().display_name())
        .collect();
    if deselected.is_empty() {
        return claim;
    }
    format!("{claim} · {} not selected", deselected.join(", "))
}

/// The query box, or the query that is still narrowing the list.
///
/// The query is bounded on entry, and bounded again here: the header must
/// never grow at the expense of the table the query exists to narrow.
///
/// Recorded departure: the prototype keeps an input field with a placeholder
/// in the pane header at all times (spec/tui-prototype.html:774). That is a
/// mouse affordance; here the filter's affordance is the `/` hint in the key
/// bar, and this line exists only while a query is being typed or is still
/// narrowing the list — a permanently drawn box a terminal cannot click
/// would advertise a control that is not there.
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
///
/// Recorded departure: the prototype's grid has an UPDATE column
/// (spec/tui-prototype.html:776). Update checking is not implemented, and a
/// column stating `current`, `available`, or `blocked` would be a claim the
/// code cannot produce, so the column does not exist until it can.
///
/// Recorded departure (skilled-hjo): the prototype also fills this row
/// (`.grid-head`, spec/tui-prototype.html:235: `background: #0b1016`), two
/// hex steps off the terminal ground it sits on (`.terminal`, #0b0f14). A
/// terminal cell cannot whisper that quietly — carrying the fill would mean
/// a new surface role indistinguishable from [`theme::TERMINAL`] — so the
/// fill is omitted: the faint uppercase and, on a tall terminal, the closing
/// rule beneath this row carry the heading's boundary instead.
fn inventory_column_headings(columns: InventoryColumns) -> Line<'static> {
    let mut cells = vec![
        padded("SKILL", columns.skill),
        padded("SOURCE", columns.source),
    ];
    for (label, width) in ["CLAUDE", "CODEX", "OPENCODE"]
        .into_iter()
        .zip(columns.agent_widths())
    {
        cells.push(padded(label, width));
    }
    cells.push("HEALTH".to_owned());
    let mut spans = vec![Span::raw(" ".repeat(ROW_MARKER_WIDTH))];
    spans.extend(components::grid_cells(
        cells
            .into_iter()
            .map(|cell| Span::styled(cell, theme::pane_subtitle()))
            .collect(),
        columns.chrome,
    ));
    Line::from(spans)
}

fn inventory_row_line(
    row: &InventoryRow,
    columns: InventoryColumns,
    selected: bool,
    width: u16,
) -> Line<'static> {
    let provenance = row.provenance();
    let source = padded(&terminal_safe(provenance.label()), columns.source);
    let mut cells = vec![
        // The name is the row's identity, set off as the prototype's
        // `.skill-name` weight sets it; the colour stays the row's own.
        Span::styled(
            padded(&terminal_safe(row.name()), columns.skill),
            theme::row_title(),
        ),
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
    for (agent, width) in AgentKind::ALL.into_iter().zip(columns.agent_widths()) {
        let observation = row.observation(agent);
        let tone = observation.map_or(Tone::Inactive, |observation| {
            installation_tone(observation.health())
        });
        // A present observation names its health beside the glyph when the
        // column can hold the words. An absent one keeps the bare `-` at
        // every width: "not installed" would outrun what an unscanned root
        // backs. The scan-scope half of that reading lives in the subtitle —
        // whose last clause names any deselected agent
        // (`qualified_inventory_subtitle`) — and in the detail region, which
        // keeps NOT INSTALLED, NO ROOT, and NOT READ apart.
        let cell = match observation {
            Some(observation) if columns.labeled => {
                format!(
                    "{} {}",
                    components::tone_glyph(tone),
                    observation.health().label()
                )
            }
            _ => components::tone_glyph(tone).to_owned(),
        };
        cells.push(Span::styled(padded(&cell, width), theme::tone_style(tone)));
    }
    let verdict = row.verdict();
    cells.push(components::badge(verdict_tone(verdict), verdict.label()));
    // The rules stand in the same columns the headings put them: both lines
    // interleave the same chrome between the same padded widths.
    let spans = components::grid_cells(cells, columns.chrome);
    // `width` is the whole table region, not the width the capped columns
    // happen to use, so the selection band crosses the slack rather than
    // stopping where the health badge does.
    components::list_row(spans, selected, width)
}

/// The tone of a row's verdict.
///
/// The two verdicts the effective resolution adds take the tones their
/// severity already carries elsewhere: a conflicting duplicate is critical
/// because an agent would resolve content nobody chose, and foreign exposure
/// is a warning because usability is uncertain rather than lost.
fn verdict_tone(verdict: RowVerdict) -> Tone {
    match verdict {
        RowVerdict::NotASkill => Tone::Inactive,
        RowVerdict::Healthy => Tone::Healthy,
        RowVerdict::Unverified | RowVerdict::Unmanaged => Tone::Unmanaged,
        RowVerdict::IncompatibleVariant | RowVerdict::ForeignVariant => Tone::Warning,
        RowVerdict::Broken | RowVerdict::Conflict => Tone::Critical,
    }
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

/// The way out of having no agent chosen, where there is one.
///
/// Rerunning setup is the only way to choose an agent, and a degraded session
/// refuses it — `can_rerun_setup` is what `src/input.rs` filters the key on.
/// Naming it anyway would send the user to a dialog that says it is
/// unavailable, so the sentence is dropped rather than reworded: the empty
/// state's job is to say what was observed, and Settings already explains why
/// the way out is closed.
fn choose_an_agent_sentence(app: &SkilledApp) -> &'static str {
    if app.can_rerun_setup() {
        " Rerun setup from Settings to choose an agent."
    } else {
        ""
    }
}

/// What a degraded session is actually withholding.
///
/// Writes always. The registry only when it was in fact lost: `open_metadata`
/// recovers its units independently, so a session degraded by a malformed
/// completion flag can hold a registry Sources still counts and Doctor still
/// draws a verdict from. Claiming those are withheld would contradict both.
fn withheld_claims_sentence(app: &SkilledApp) -> &'static str {
    if app.inventory().registry_is_complete() {
        "Every write is withheld for this session."
    } else {
        "Registry-backed claims and every write are withheld for this session."
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
            format!(
                "Skilled reads the skill root of the agents chosen during setup, \
                 and none are chosen, so it read nothing.{}",
                choose_an_agent_sentence(app)
            ),
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
    // The degraded session qualifies the one answer it changes the meaning of
    // — roots read whole, holding nothing — because a reader is entitled to
    // know that emptiness was observed rather than inferred from metadata.
    // Every other answer is the scan's alone: `metadata_failure_line` sits
    // above this table either way, and a session that learned nothing else
    // about the roots would be trading its only filesystem result for a
    // sentence already on screen. Doctor orders these the same way.
    if app.metadata_failure().is_some()
        && roots
            .iter()
            .any(|root| matches!(root.status(), RootStatus::Scanned { .. }))
    {
        let scope = if app.scan_scope_known() {
            "Skilled retained the agent selection and scanned its selected roots read-only."
        } else {
            "Skilled scanned every detected agent root read-only."
        };
        return (
            "No skills are installed".to_owned(),
            format!(
                "The agent skill roots Skilled read hold no skill directories. {scope} {}",
                withheld_claims_sentence(app)
            ),
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

/// Doctor: every finding the last scan holds, and what one of them is about.
fn render_doctor(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp, findings: &[DoctorItem<'_>]) {
    match viewport::workspace_regions(area) {
        (primary, Some(detail)) => {
            render_doctor_findings(frame, primary, app, findings);
            render_doctor_detail(frame, detail, app, findings, true);
        }
        (primary, None) => match app.doctor_pane() {
            DoctorPane::Findings => render_doctor_findings(frame, primary, app, findings),
            DoctorPane::Details => render_doctor_detail(frame, primary, app, findings, false),
        },
    }
}

/// Column widths for the findings table.
///
/// The severity badge and the agent are sized by their longest value, which
/// never changes; the code and the skill divide what is left. The code is the
/// finding's identity and takes the larger share, capped where a longer field
/// would only add whitespace.
#[derive(Clone, Copy)]
struct DoctorColumns {
    code: usize,
    skill: usize,
}

/// Wide enough for `× critical` and a column of clearance.
const SEVERITY_COLUMN_WIDTH: usize = 11;
/// Wide enough for `Claude Code` and a column of clearance.
const DOCTOR_AGENT_WIDTH: usize = 12;
/// The longest stable codes this release can produce are the OpenCode
/// exposure codes, at thirty-three cells.
const MAX_FINDING_CODE_WIDTH: usize = 34;
const MINIMUM_FINDING_CODE_WIDTH: usize = 12;

fn doctor_columns(width: u16) -> DoctorColumns {
    let fixed = ROW_MARKER_WIDTH + SEVERITY_COLUMN_WIDTH + DOCTOR_AGENT_WIDTH;
    let remaining = usize::from(width).saturating_sub(fixed);
    // Seven tenths, not six: the code is the finding's identity, and the cap
    // is exactly the longest code this release can produce, so at the minimum
    // supported width every code is shown whole rather than ellipsized down to
    // a prefix two codes could share.
    let code = (remaining * 7 / 10).clamp(MINIMUM_FINDING_CODE_WIDTH, MAX_FINDING_CODE_WIDTH);
    let skill = remaining
        .saturating_sub(code)
        .clamp(MINIMUM_SKILL_WIDTH, MAX_SKILL_WIDTH);
    DoctorColumns { code, skill }
}

fn render_doctor_findings(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &SkilledApp,
    findings: &[DoctorItem<'_>],
) {
    let body = if let Some(line) = metadata_failure_line(app, area.width) {
        render_pane_scaffold_with_status(
            frame,
            area,
            "Doctor",
            &doctor_subtitle(app, findings.len()),
            app.doctor_pane() == DoctorPane::Findings,
            line,
        )
    } else {
        render_pane_scaffold(
            frame,
            area,
            "Doctor",
            &doctor_subtitle(app, findings.len()),
            app.doctor_pane() == DoctorPane::Findings,
            false,
        )
    };

    if findings.is_empty() {
        let region = body.inner(Margin {
            horizontal: 2,
            vertical: 0,
        });
        let (glyph, headline, explanation) = doctor_empty_state(app);
        frame.render_widget(
            components::empty_state(glyph, &headline, &explanation, region),
            region,
        );
        return;
    }

    let columns = doctor_columns(body.width);
    let mut lines = vec![doctor_column_headings(columns)];
    let capacity = usize::from(body.height.max(1)).saturating_sub(1);
    let start = visible_window_start(app.focused_finding(), capacity);
    lines.extend(
        findings
            .iter()
            .enumerate()
            .skip(start)
            .take(capacity)
            .map(|(index, entry)| {
                doctor_row_line(entry, columns, index == app.focused_finding(), body.width)
            }),
    );
    frame.render_widget(Paragraph::new(lines), body);
}

/// What the findings pane can honestly say it holds.
///
/// The count is the snapshot's to give, exactly as the Inventory's is, so the
/// tab and this subtitle cannot disagree about whether a number may be stated.
fn doctor_subtitle(app: &SkilledApp, listed: usize) -> String {
    let inventory = app.inventory();
    if inventory.scan_pending() {
        return "not scanned".to_owned();
    }
    if inventory.no_agent_configured() {
        return "no root read".to_owned();
    }
    // A withheld count is withheld for one of two reasons, and they are
    // different answers: part of the requested scope could not be read, or
    // none of it was read at all. Registry-side findings exist without any
    // root being read, so the second is reachable with a non-empty list and
    // must be settled before the list's own size is spoken about.
    //
    // The metadata failure is one of the withholding reasons rather than a
    // reason of its own: the count is the snapshot's to give, and a session
    // whose registry survived a malformed completion flag can still give one.
    // The banner above this subtitle states the failure either way, so a
    // stateable number is never spent on repeating it.
    let degraded = app.metadata_failure().is_some();
    match app.stated_finding_count() {
        Some(0) => "nothing to report".to_owned(),
        Some(1) => "1 finding".to_owned(),
        Some(findings) => format!("{findings} findings"),
        None if inventory.unreadable_roots().next().is_some() && listed > 0 => {
            format!("{listed} listed · not fully read")
        }
        // The roots are settled before the registry, in the order
        // `doctor_empty_state` states them, so the pane header and the body
        // beneath it lead with the same reason.
        None if inventory.unreadable_roots().next().is_some() => "not fully read".to_owned(),
        None if !read_a_root(inventory) && listed > 0 => format!("{listed} listed · no root read"),
        None if !read_a_root(inventory) => "no root read".to_owned(),
        // Behind every answer the roots have, for the same reason the empty
        // state puts it last: this one is already on screen in the banner. And
        // narrowed the same way `doctor_empty_state` narrows it, so the pane
        // header and the body beneath it lead with the same reason: a session
        // degraded by a malformed completion flag can still hold a registry
        // that was read whole, and there the receipt table is what is actually
        // missing.
        None if degraded && !inventory.registry_is_complete() && listed > 0 => {
            format!("{listed} listed · metadata unavailable")
        }
        None if degraded && !inventory.registry_is_complete() => "metadata unavailable".to_owned(),
        None if !inventory.registry_is_complete() && listed > 0 => {
            format!("{listed} listed · a source could not be read")
        }
        None if !inventory.registry_is_complete() => "a source could not be read".to_owned(),
        None if listed > 0 => format!("{listed} listed · receipts could not be read"),
        None => "receipts could not be read".to_owned(),
    }
}

/// Whether any root was actually read, as opposed to accounted for.
///
/// Finding every root absent is a complete answer about the roots and no
/// reading of any of them; the difference is what several of Doctor's
/// withholding phrases turn on.
fn read_a_root(inventory: &crate::inventory::InventorySnapshot) -> bool {
    inventory
        .roots()
        .iter()
        .any(|root| matches!(root.status(), RootStatus::Scanned { .. }))
}

fn doctor_column_headings(columns: DoctorColumns) -> Line<'static> {
    let mut heading = " ".repeat(ROW_MARKER_WIDTH);
    heading.push_str(&padded("SEVERITY", SEVERITY_COLUMN_WIDTH));
    heading.push_str(&padded("FINDING", columns.code));
    heading.push_str(&padded("SKILL", columns.skill));
    heading.push_str("AGENT");
    Line::from(Span::styled(heading, theme::pane_subtitle()))
}

fn doctor_row_line(
    entry: &DoctorItem<'_>,
    columns: DoctorColumns,
    selected: bool,
    width: u16,
) -> Line<'static> {
    let severity = entry.finding().severity();
    let tone = severity_tone(severity);
    let badge = components::badge(tone, severity.label());
    let padding = SEVERITY_COLUMN_WIDTH.saturating_sub(badge.width());
    let spans = vec![
        badge,
        Span::raw(" ".repeat(padding)),
        Span::raw(padded(entry.finding().code(), columns.code)),
        Span::raw(padded(&terminal_safe(entry.skill_name()), columns.skill)),
        Span::raw(entry.agent_option().map_or("", AgentKind::display_name)),
    ];
    components::list_row(spans, selected, width)
}

fn severity_tone(severity: FindingSeverity) -> Tone {
    match severity {
        FindingSeverity::Info => Tone::Inactive,
        FindingSeverity::Warning => Tone::Warning,
        FindingSeverity::Critical => Tone::Critical,
    }
}

/// What an empty findings list can honestly say, given what the scan observed.
///
/// The cases the Inventory keeps apart are kept apart here for the same
/// reason: a clean bill of health and an unread root are not the same answer.
/// Only the first earns the tick — every other empty list is empty because
/// something was not read, and a tick over one of those would be the whole
/// mistake this function exists to avoid.
fn doctor_empty_state(app: &SkilledApp) -> (&'static str, String, String) {
    let inventory = app.inventory();
    if inventory.scan_pending() {
        return (
            "·",
            "Installation roots have not been scanned".to_owned(),
            "Skilled scans the roots when this view opens.".to_owned(),
        );
    }
    if inventory.no_agent_configured() {
        return (
            "·",
            "No agent is configured".to_owned(),
            format!(
                "Skilled reads the skill root of the agents chosen during setup, \
                 and none are chosen, so it read nothing.{}",
                choose_an_agent_sentence(app)
            ),
        );
    }
    if inventory.unreadable_roots().next().is_some() {
        return (
            "·",
            "An agent skill root could not be read".to_owned(),
            "Skilled reports nothing from a root it could not read in full \
             rather than reporting part of it, so this list covers less than \
             the roots it was asked to look at."
                .to_owned(),
        );
    }
    // After the scan's own answers, because Doctor lists what was observed and
    // the metadata banner already states the failure above this body. A root
    // that could not be read is named nowhere else on this screen, so the
    // degraded session must not take its place.
    //
    // And only where the registry was in fact lost with it. `open_metadata`
    // recovers its units independently, so a session degraded by a malformed
    // completion flag can hold a registry that was read whole — saying it
    // cannot be claimed complete would be untrue of the very thing this
    // sentence names.
    // Nothing was read, so nothing may be said about what is installed. The
    // roots were all accounted for — that is why no count was withheld for
    // them — but accounting for a root that is absent is not reading one.
    if !read_a_root(inventory) {
        return (
            "·",
            "No agent skill root exists yet".to_owned(),
            "Skilled looked for the documented global skill root of each selected \
             agent and found none of them, so it read nothing to report on. It \
             did not create one."
                .to_owned(),
        );
    }
    if app.metadata_failure().is_some() && !inventory.registry_is_complete() {
        return (
            "·",
            "Application metadata is unavailable".to_owned(),
            "Skilled kept the read-only scan and diagnosis available, but it cannot claim the \
             registry is complete or perform writes in this session."
                .to_owned(),
        );
    }
    // The registry is the findings' second source, and a source that could not
    // be read may hold the very variant that would have made a name ambiguous.
    if !inventory.registry_is_complete() {
        return (
            "·",
            "A registered source could not be read".to_owned(),
            "Every root Skilled read is accounted for, but a registered source \
             is not, so it cannot say whether two variants compete for a name. \
             Sources names the repository that could not be read."
                .to_owned(),
        );
    }
    if !app.repair_receipts_readable() {
        return (
            "·",
            "Ownership receipts could not be read".to_owned(),
            "The installation roots and registry were read, but Skilled cannot state repairability or a complete finding count without its ownership evidence."
                .to_owned(),
        );
    }
    (
        "✓",
        "Nothing to report".to_owned(),
        "Every installation Skilled read resolves to the variant it came from, \
         and no registered variant competes with another."
            .to_owned(),
    )
}

/// The detail region: everything observed about the selected finding.
fn render_doctor_detail(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &SkilledApp,
    findings: &[DoctorItem<'_>],
    beside_the_table: bool,
) {
    let selected = findings.get(app.focused_finding());
    let body = render_detail_scaffold(
        frame,
        area,
        "Details",
        &selected.map_or_else(
            || "no selection".to_owned(),
            |entry| terminal_safe(entry.skill_name()),
        ),
        app.doctor_pane() == DoctorPane::Details,
        beside_the_table,
        false,
    );

    let Some(entry) = selected else {
        frame.render_widget(
            components::empty_state(
                "·",
                "Nothing to show",
                "What was observed, why it weakens usability, and every path \
                 involved appear here once a finding is selected.",
                body,
            ),
            body,
        );
        return;
    };
    render_detail_window(
        frame,
        body,
        doctor_detail_lines(app, entry, body.width),
        app.detail_scroll(),
        rows_below_advice(app),
    );
}

fn doctor_detail_lines(app: &SkilledApp, entry: &DoctorItem<'_>, width: u16) -> Vec<Line<'static>> {
    let home = app.home();
    let severity = entry.finding().severity();
    let mut lines = Vec::new();
    push_detail_section(&mut lines, "FINDING", width);
    lines.push(Line::styled(entry.finding().code(), theme::pane_heading()));
    lines.push(Line::from(components::badge(
        severity_tone(severity),
        severity.label(),
    )));
    // The pane header above already names the skill; the agent is named
    // nowhere else in the region and a finding that did not say which agent it
    // concerns would be half a finding.
    if let Some(agent) = entry.agent_option() {
        lines.push(detail_field("Agent", agent.display_name()));
    } else if let Some(source) = entry.source() {
        lines.push(detail_field("Source", &terminal_safe(source.label())));
        lines.push(detail_field(
            "Path",
            &terminal_safe(&source.git_top_level().display().to_string()),
        ));
    }

    push_detail_section(&mut lines, "OBSERVED", width);
    lines.push(Line::from(Span::raw(terminal_safe_bounded_start(
        entry.finding().evidence(),
        usize::from(width).saturating_mul(6),
    ))));

    push_detail_section(&mut lines, "CONSEQUENCE", width);
    lines.push(Line::from(finding_consequence(entry)));

    if let Some(observation) = entry.observation() {
        push_detail_section(&mut lines, "INSTALLATION", width);
        lines.push(detail_field_bounded(
            "Path",
            &home_relative(observation.path(), home),
            width,
            2,
        ));
        lines.push(detail_field("Object", observation.object().description()));
        if let InstallationObject::Symlink { target } = observation.object()
            && !target.as_os_str().is_empty()
        {
            lines.push(detail_field_bounded(
                "Target",
                &home_relative(target, home),
                width,
                2,
            ));
        }
        if let Some(resolution) = observation.resolution() {
            lines.push(detail_field_bounded(
                "Variant",
                &resolution.evidence_label(),
                width,
                3,
            ));
        }
    }
    if !entry.variants().is_empty() {
        push_detail_section(&mut lines, "COMPETING VARIANTS", width);
        lines.extend(
            entry.variants().iter().map(|variant| {
                detail_field_bounded("Variant", &variant.evidence_label(), width, 3)
            }),
        );
    }

    let repair = entry.observation().map_or_else(
        || "not offered: this finding does not concern one installed link".to_owned(),
        |observation| match app.repair_offer(observation.path()) {
            RepairOfferStatus::Offered => {
                "offered: Skilled holds an exact matching receipt; press r to preview".to_owned()
            }
            RepairOfferStatus::NotOffered { reason } => {
                format!("not offered: {}", terminal_safe(&reason))
            }
        },
    );
    lines.push(detail_field("Repair", &repair));
    lines
}

/// Why a finding weakens usability, in one sentence.
///
/// Settled per code rather than per severity: `unmanaged` and `benign_alias`
/// are both informational and mean entirely different things, and a reader who
/// has just been shown a code deserves to be told what it costs them.
///
/// `variant.duplicate_for_agent` is the one code that carries two findings.
/// Filed against what is installed, precedence does pick one and the complaint
/// is that the other is unreachable; filed against the registry, nothing has
/// picked anything and the complaint is that nothing can. Keying on the code
/// alone would state one of those beside the evidence for the other.
///
/// [`DoctorItem::concerns_the_registry`] is what tells them apart. Neither
/// finding hangs off an installation — an effective resolution is a fact about
/// several roots at once, so it is filed on the row — so the observation cannot
/// be asked; the competing variants can.
fn finding_consequence(entry: &DoctorItem<'_>) -> &'static str {
    match entry.finding().code() {
        "variant.duplicate_for_agent" if !entry.concerns_the_registry() => {
            "The highest-precedence root wins and the other definition is never \
             loaded, whichever one was meant."
        }
        "install.dangling_symlink" => {
            "The agent finds a link with nothing behind it, so the skill does not load."
        }
        "install.wrong_managed_target" => {
            "The skill loads, but this agent now selects a different registered variant under the same name."
        }
        "install.unresolvable_symlink" | "install.unreadable_entry" => {
            "Skilled could not follow what is installed here, so what the agent loads \
             is not known."
        }
        "install.unmanaged" => {
            "The skill loads. Skilled did not place it, so it cannot say where it came \
             from or keep it current."
        }
        "install.not_a_skill" => {
            "An agent ignores this entry. It is listed so it is not mistaken for a skill \
             that failed to install."
        }
        "install.provenance_unverified" => {
            "Skilled could not read its registry in full, so it cannot say whether this came \
             from a registered source."
        }
        "variant.duplicate_for_agent" => {
            "Which definition the agent would resolve is not something Skilled can \
             state, so installing under this name is blocked until a source is chosen."
        }
        "variant.foreign_opencode_exposure" => {
            "OpenCode can see another agent's edition of this skill and no edition of \
             its own. Skilled does not claim one agent's edition is usable by another."
        }
        "variant.incompatible_for_opencode" => {
            "OpenCode can reach this registered variant, but its catalog excludes OpenCode. \
             Skilled therefore does not claim the variant is usable by it."
        }
        "variant.benign_alias" => {
            "Nothing is wrong: every root reaches one directory, so one skill loads."
        }
        code if code.starts_with("skill.") => {
            "An agent cannot load a skill whose SKILL.md fails the portable core."
        }
        "source.dirty" => {
            "The checkout has changes of its own, so Skilled will not advance it over them."
        }
        "source.diverged" => {
            "Local commits are not on the upstream branch, so no fast-forward exists. \
             Skilled does not rebase or merge."
        }
        "source.missing" => {
            "The registered checkout could not be read, so nothing can be said about what \
             it holds or whether it is current."
        }
        "source.detached_head" => {
            "HEAD is not on a branch here, so there is no branch for a fast-forward to \
             advance."
        }
        "source.no_upstream" => {
            "The branch tracks nothing, so there is no upstream to check against or to \
             fast-forward to."
        }
        "source.upstream_unfetched" => {
            "The branch tracks an upstream whose remote-tracking ref is not here, so a check \
             has to fetch it before an update can be judged."
        }
        "source.fetch_failed" => {
            "The check could not reach the upstream, so whether an update exists is not \
             known."
        }
        "source.partial_clone_unsupported" => {
            "Git may fetch missing objects here outside an explicit check, so Skilled does \
             not update this repository."
        }
        "source.repository_transport_unsupported" => {
            "This checkout configures a program for Git to run while fetching, so checking \
             it would run code the repository chose rather than only reading."
        }
        "source.submodule_update_unsupported" => {
            "Advancing this checkout would move a submodule Skilled does not manage, so the \
             update is refused."
        }
        "source.removal_leaves_content" => {
            "The update takes a skill away but leaves its directory standing, so the \
             installation would resolve to something that is not a skill rather than losing \
             its target."
        }
        "source.revival_name_mismatch" => {
            "The link uses a different installation name, so the updated skill would still \
             fail validation there."
        }
        "source.changed_after_preview" => {
            "The repository moved after its plan was previewed, so that plan was abandoned \
             without writing. Check again for the current state."
        }
        "update.apply_failed" => {
            "Git failed after Skilled reached the fast-forward command, so the checkout was \
             rescanned but the write is not reported as applied or verified."
        }
        "update.verification_failed" => {
            "A fast-forward was applied and the result disagreed with the plan, so what this \
             source holds is not what was agreed to."
        }
        "update.verification_incomplete" => {
            "Something the plan promised could not be re-read after the fast-forward, so the \
             update is not reported as verified."
        }
        _ => "Skilled has no account of what this costs.",
    }
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

/// The heading and its rule, which a flush pane spends before its body.
const PANE_HEADER_HEIGHT: u16 = 2;

/// The header block of a pane that carries the prototype's `.pane-header`
/// clearance (spec/tui-prototype.html:167): a blank row above the heading,
/// and a bottom-edge underline — not a centred rule — on the row beneath it,
/// so the border's ink sits one row below the heading the way the blank row
/// sits one row above, and the heading reads centered. The clearance is
/// inside the pane rather than cut from the workspace, so a separator beside
/// the pane runs through it to the bar above. The Inventory's panes carry it;
/// the other views' panes still sit flush, so the choice is the pane's rather
/// than the scaffold's.
const PADDED_PANE_HEADER_HEIGHT: u16 = 3;

/// The header and body a pane's area divides into.
///
/// Shared so a caller that has to measure a body it is not drawing — the
/// scroll extent the detail region reports — divides the area exactly as the
/// scaffold that draws it does.
fn pane_regions(area: Rect, padded: bool) -> [Rect; 2] {
    let header = if padded {
        PADDED_PANE_HEADER_HEIGHT
    } else {
        PANE_HEADER_HEIGHT
    };
    Layout::vertical([Constraint::Length(header), Constraint::Min(1)]).areas(area)
}

/// A workspace pane: its header, the rule that closes it, and the body left
/// for the pane's own content. `padded` keeps the clearance
/// [`PADDED_PANE_HEADER_HEIGHT`] describes.
fn render_pane_scaffold(
    frame: &mut Frame<'_>,
    area: Rect,
    heading: &str,
    subtitle: &str,
    focused: bool,
    padded: bool,
) -> Rect {
    let [header, body] = pane_regions(area, padded);
    let mut lines = Vec::with_capacity(usize::from(PADDED_PANE_HEADER_HEIGHT));
    if padded {
        lines.push(Line::default());
    }
    lines.push(pane_header(heading, subtitle, focused, header.width));
    lines.push(if padded {
        components::underline(header.width)
    } else {
        components::rule(header.width)
    });
    frame.render_widget(Paragraph::new(lines), header);
    body
}

/// A Doctor header with the session-wide metadata failure kept above its
/// finding rows rather than synthesised as a skill finding.
fn render_pane_scaffold_with_status(
    frame: &mut Frame<'_>,
    area: Rect,
    heading: &str,
    subtitle: &str,
    focused: bool,
    status: Line<'static>,
) -> Rect {
    let mut lines = vec![pane_header(heading, subtitle, focused, area.width), status];
    let height = detail_lines_height(&lines, area.width)
        .saturating_add(1)
        .min(usize::from(area.height.saturating_sub(1)));
    let [header, body] = Layout::vertical([
        Constraint::Length(u16::try_from(height).unwrap_or(u16::MAX)),
        Constraint::Min(1),
    ])
    .areas(area);
    lines.push(components::rule(header.width));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), header);
    body
}

/// The banner a degraded session carries above whatever it managed to read.
///
/// The path and the operating-system cause both come from outside Skilled and
/// have no length either it or the user controls. Unbounded, a long enough
/// application-data path wraps until the header takes the pane and hides the
/// read-only inventory this whole mode exists to keep showing.
///
/// They are bounded apart, two rows each, because they fail differently. A
/// path loses its middle, where a deep tree says least and both ends name the
/// file; a cause is a sentence and loses its end. Bounding the two together
/// would spend the whole budget on a long path and cut the reason off
/// entirely, which is the half the user can act on. Two rows is past the
/// length either reaches in practice — the bound is there to stop a
/// pathological one, not to shorten an ordinary one. The scope sentence is
/// Skilled's own and known short, so it is stated whole.
fn metadata_failure_line(app: &SkilledApp, width: u16) -> Option<Line<'static>> {
    let failure = app.metadata_failure()?;
    let scope = if app.scan_scope_known() {
        "The agent selection was retained; selected roots were scanned read-only. \
         Writes are refused."
    } else {
        "The agent selection could not be read; all detected roots were scanned read-only. \
         Writes are refused."
    };
    let badge = components::badge(Tone::Critical, "metadata unavailable");
    let budget = usize::from(width)
        .saturating_mul(2)
        .saturating_sub(badge.width() + 2);
    let path = terminal_safe_bounded_middle(&failure.database_path().display().to_string(), budget);
    let cause = terminal_safe_bounded_start(failure.cause(), budget);
    Some(Line::from(vec![
        badge,
        Span::raw(format!(": {path}: {cause}. {scope}")),
    ]))
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
            .saturating_sub(ROW_MARKER_WIDTH)
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
    padded: bool,
) -> Rect {
    let region = detail_regions(area, beside_the_primary_region);
    if let Some(separator) = region.separator {
        render_region_separator(frame, separator);
    }
    // Painted whole, before the margin: the surface is what makes the region
    // read as a region, so it reaches the edges the text does not.
    frame.render_widget(Block::new().style(theme::detail_surface()), region.surface);
    render_pane_scaffold(frame, region.text, heading, subtitle, focused, padded)
}

/// The rectangles a detail region is built from.
///
/// Pure, and the only description of the region's geometry, so the extent the
/// frame reports for a body is measured against the body that was drawn.
struct DetailRegions {
    /// The dividing rule, when the region sits beside a primary one.
    separator: Option<Rect>,
    /// Everything the region's surface colour reaches.
    surface: Rect,
    /// The surface inside its text margin: header, rule, and body.
    text: Rect,
}

impl DetailRegions {
    /// The rows the region's own lines get, below the header and its rule.
    /// `padded` must match what the scaffold drew, or the extent measured is
    /// not the extent shown.
    fn body(&self, padded: bool) -> Rect {
        pane_regions(self.text, padded)[1]
    }
}

fn detail_regions(area: Rect, beside_the_primary_region: bool) -> DetailRegions {
    let (separator, surface) = if beside_the_primary_region {
        let [separator, region] =
            Layout::horizontal([Constraint::Length(1), Constraint::Min(1)]).areas(area);
        (Some(separator), region)
    } else {
        (None, area)
    };
    DetailRegions {
        separator,
        surface,
        text: surface.inner(Margin {
            horizontal: 1,
            vertical: 0,
        }),
    }
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
        true,
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
    // dropped off the bottom without a trace — and, where the region has the
    // keyboard, reached by scrolling rather than only reported.
    let overlay_findings = row
        .observations()
        .filter_map(|observation| {
            app.repair_overlay_finding(observation.path())
                .map(|finding| (observation.agent(), finding))
        })
        .collect::<Vec<_>>();
    let lines = inventory_detail_lines(
        row,
        app.inventory().roots(),
        app.home(),
        body.width,
        &overlay_findings,
    );
    render_detail_window(
        frame,
        body,
        lines,
        app.detail_scroll(),
        rows_below_advice(app),
    );
}

/// What the region can honestly tell a reader about reaching the rows below
/// its window, from wherever the keyboard currently is.
///
/// A dialog answers for the keyboard while it is open, and the filter bar
/// takes every printable key for its query: under either, no keystroke this
/// notice could name would reach anything. Both screens say as much elsewhere
/// — the help overlay locks navigation, the filter bar says so on the
/// navigation row — and a region contradicting them from underneath is the
/// worse of the two claims, because it is the one about the rows in question.
/// The way out of both is `Esc`, which is not a scroll and is not named here.
fn rows_below_advice(app: &SkilledApp) -> RowsBelow {
    let focused = match app.view() {
        View::Inventory => app.inventory_pane() == InventoryPane::Details,
        View::Doctor => app.doctor_pane() == DoctorPane::Details,
        View::Updates => app.updates_pane() == UpdatesPane::Details,
        _ => false,
    };
    if app.help_context().is_some() || app.inventory_filter_active() {
        RowsBelow::NotFromHere
    } else if focused {
        RowsBelow::UnderTheseKeys
    } else {
        RowsBelow::BehindTheFocus
    }
}

/// Draw a scrollable detail region, accounting for every row it does not show.
///
/// The window is described first and drawn second, so the notices at either
/// end are measured from the same arithmetic that decides what is visible: the
/// rows claimed above, the rows on screen, and the rows claimed below always
/// add up to the content the region holds.
///
/// The offset is clamped here as well as in the reducer. The reducer can only
/// clamp against what the previous frame measured, and a terminal that shrank
/// since then would otherwise scroll the region past its content and show a
/// blank body — an emptiness the user would read as an absence of content.
fn render_detail_window(
    frame: &mut Frame<'_>,
    body: Rect,
    lines: Vec<Line<'static>>,
    offset: usize,
    advice: RowsBelow,
) {
    if body.width == 0 {
        return;
    }
    let rows_per_line: Vec<usize> = lines
        .iter()
        .map(|line| wrapped_line_count(line, body.width))
        .collect();
    let window = detail_window(&rows_per_line, body.height, offset);
    // The slack sits between the content and the notice below it: the notice
    // belongs at the region's edge, where a reader looks for the end, and the
    // rows a whole line could not fill are the end of what is shown.
    let [above, content, _slack, below] = Layout::vertical([
        Constraint::Length(u16::from(window.above > 0)),
        Constraint::Length(u16::try_from(window.shown).unwrap_or(u16::MAX)),
        Constraint::Min(0),
        Constraint::Length(u16::from(window.below > 0)),
    ])
    .areas(body);

    if window.above > 0 {
        frame.render_widget(
            Paragraph::new(rows_above_notice(window.above, body.width)),
            above,
        );
    }
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(window.above).unwrap_or(u16::MAX), 0)),
        content,
    );
    if window.below > 0 {
        // A region too short to scroll answers to no keystroke, whatever the
        // caller had in mind for one that can. No terminal the shell agrees to
        // draw in is that short, so nothing on screen can reach this: it is
        // here so the advice is a property of the region rather than of the
        // floor that currently protects it.
        let advice = if detail_max_scroll(&rows_per_line, body.height) == 0 {
            RowsBelow::NotFromHere
        } else {
            advice
        };
        frame.render_widget(
            Paragraph::new(rows_below_notice(window.below, body.width, advice)),
            below,
        );
    }
}

/// How a detail region spends its rows at one offset: what it has scrolled
/// past, what it shows, and what is still below.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DetailWindow {
    above: usize,
    shown: usize,
    below: usize,
}

/// Divide a region's rows between the content and the notices at its ends.
///
/// The notices cost a row each, and they are counted before the content rather
/// than after it: a notice that had to be squeezed in afterwards would either
/// wrap off the bottom or push away the row it was reporting on. The result
/// drives the layout as well as the counts, so what the region says it showed
/// is the height it was given to show it in.
///
/// The window moves and stops by whole lines, though it counts in rows.
///
/// Rows are the honest unit for a count — a hidden line that would have
/// wrapped costs the reader more than one of them — but they are the wrong
/// unit to move by here. A window that opened mid-line would show the tail of
/// a path with its label scrolled off, and one that closed mid-line would show
/// a `Target:` with nothing after it: this region's whole job is to state what
/// was observed, and half a field states something that was not. Moving by
/// lines also keeps every keystroke worth pressing, where snapping only the
/// bottom edge left steps that consumed a row at the top and revealed none at
/// the foot.
///
/// The price is up to a line's worth of blank rows above the notice, where the
/// next line does not fit in what is left. A line taller than the whole window
/// is the exception that has nowhere to stop: it is shown in part, because
/// withholding it would leave the region blank.
fn detail_window(rows_per_line: &[usize], height: u16, offset: usize) -> DetailWindow {
    let rows = usize::from(height);
    let total_rows: usize = rows_per_line.iter().sum();
    let first = offset.min(detail_max_scroll(rows_per_line, height));
    let above: usize = rows_per_line.iter().take(first).sum();
    let remaining = total_rows.saturating_sub(above);
    let reserved = usize::from(above > 0);
    if remaining <= rows.saturating_sub(reserved) {
        return DetailWindow {
            above,
            shown: remaining,
            below: 0,
        };
    }
    let capacity = rows.saturating_sub(reserved).saturating_sub(1);
    let shown = whole_lines_within(&rows_per_line[first.min(rows_per_line.len())..], capacity)
        .unwrap_or(capacity);
    DetailWindow {
        above,
        shown,
        below: remaining.saturating_sub(shown),
    }
}

/// The rows of as many whole lines as `capacity` holds, or `None` where the
/// first of them alone outgrows it.
fn whole_lines_within(rows_per_line: &[usize], capacity: usize) -> Option<usize> {
    let mut used = 0;
    let mut shown = None;
    for line_rows in rows_per_line {
        if used + line_rows > capacity {
            break;
        }
        used += line_rows;
        shown = Some(used);
    }
    shown
}

/// The furthest line the window can open on: the first one from which the rest
/// of the content fits, and no further, because a window scrolled past its
/// content shows emptiness that reads as an absence of content.
///
/// A region with fewer than two rows can hold a notice or a row of content but
/// not both, so it cannot scroll usefully and only reports what it dropped.
/// Every subtraction downstream of this one is guarded by that: the returned
/// offset is never past the last line, whatever the caller asks for.
fn detail_max_scroll(rows_per_line: &[usize], height: u16) -> usize {
    let rows = usize::from(height);
    let total_rows: usize = rows_per_line.iter().sum();
    if rows < 2 || total_rows <= rows {
        return 0;
    }
    // One row goes to the notice for the lines scrolled past, so the last
    // window has `rows - 1` in which to finish the content.
    let capacity = rows - 1;
    let mut above = 0;
    for (line, line_rows) in rows_per_line.iter().enumerate() {
        if total_rows - above <= capacity {
            return line;
        }
        above += line_rows;
    }
    // A last line taller than the window itself can be opened on but never
    // finished; stopping anywhere earlier would hide it as well.
    rows_per_line.len().saturating_sub(1)
}

/// How far the Inventory's detail region could be scrolled in this frame, or
/// `None` where this frame did not draw it.
///
/// Measured from the workspace the frame is about to lay out, so a hint or a
/// help entry gated on it describes the terminal the user is looking at. The
/// absent case is kept apart from a measured zero for the reason the scanner
/// keeps "not read" apart from "nothing there": a compact terminal showing the
/// table has not discovered that the region behind it holds nothing, and
/// answering zero would throw away an offset the user scrolled to and will
/// come back to.
fn detail_scroll_extent(
    app: &SkilledApp,
    area: Rect,
    workspace: Rect,
    findings: &[DoctorItem<'_>],
) -> Option<usize> {
    // The install dialog is drawn over the workspace and owns the window while
    // it is open, so it is measured instead of whatever is behind it. The
    // reducer keeps one offset because only one scrollable thing is ever on
    // screen, and a modal is exactly that.
    if let Some(prompt) = app.pending_operation() {
        let body = install_prompt_regions(area, 0).body;
        if body.width == 0 {
            return None;
        }
        // Counted in wrapped rows, not in lines. A detail region moves its
        // window a whole field at a time so a wrapped value never opens with
        // its label above the edge; this is one paragraph the reader is asked
        // to agree to, and `Paragraph::scroll` counts rows, so the last row of
        // it has to be reachable even when a path wraps. The offset state is
        // shared with the detail regions because only one scrollable thing is
        // ever on screen — the renderer measures the unit, and the reducer
        // only ever clamps to what it was told.
        let lines = match prompt {
            OperationPrompt::Install(prompt) => {
                install_prompt_lines(prompt, app.home(), body.width)
            }
            OperationPrompt::Uninstall(prompt) => uninstall_prompt_lines(prompt),
            OperationPrompt::Forget(prompt) => forget_prompt_lines(prompt),
        };
        let rows: usize = lines
            .iter()
            .map(|line| wrapped_line_count(line, body.width))
            .sum();
        return Some(rows.saturating_sub(usize::from(body.height)));
    }
    if let Some(prompt) = app.pending_repair() {
        let body = install_prompt_regions(area, 0).body;
        if body.width == 0 {
            return None;
        }
        let rows: usize = repair_prompt_lines(prompt, body.width)
            .iter()
            .map(|line| wrapped_line_count(line, body.width))
            .sum();
        return Some(rows.saturating_sub(usize::from(body.height)));
    }
    if let Some(prompt) = app.pending_update() {
        let body = update_prompt_regions(area, 0).body;
        if body.width == 0 {
            return None;
        }
        let rows = update_prompt_rows(prompt, body.width).len();
        return Some(rows.saturating_sub(usize::from(body.height)));
    }
    let (primary, detail) = viewport::workspace_regions(workspace);
    // `padded` mirrors what each view's scaffold draws: the Inventory's
    // panes carry the header clearance, the Doctor's do not.
    let focused_alone = |drilled_in: bool, padded: bool| match (detail, drilled_in) {
        (Some(detail), _) => Some(detail_regions(detail, true).body(padded)),
        (None, true) => Some(detail_regions(primary, false).body(padded)),
        (None, false) => None,
    };
    let (body, lines) = match app.view() {
        View::Inventory => {
            let body = focused_alone(app.inventory_pane() == InventoryPane::Details, true)?;
            if body.width == 0 {
                return None;
            }
            // A region with no selection draws an empty state rather than a
            // window, and an empty state has nothing to scroll — that is a
            // measurement, not an absence of one.
            let Some(row) = app.selected_installation() else {
                return Some(0);
            };
            let overlay_findings = row
                .observations()
                .filter_map(|observation| {
                    app.repair_overlay_finding(observation.path())
                        .map(|finding| (observation.agent(), finding))
                })
                .collect::<Vec<_>>();
            (
                body,
                inventory_detail_lines(
                    row,
                    app.inventory().roots(),
                    app.home(),
                    body.width,
                    &overlay_findings,
                ),
            )
        }
        View::Doctor => {
            let body = focused_alone(app.doctor_pane() == DoctorPane::Details, false)?;
            if body.width == 0 {
                return None;
            }
            let Some(entry) = findings.get(app.focused_finding()) else {
                return Some(0);
            };
            (body, doctor_detail_lines(app, entry, body.width))
        }
        View::Updates => {
            let body = focused_alone(app.updates_pane() == UpdatesPane::Details, false)?;
            if body.width == 0 {
                return None;
            }
            let Some(source) = app.selected_update_source() else {
                return Some(0);
            };
            (body, update_detail_lines(app, source))
        }
        _ => return None,
    };
    let rows_per_line: Vec<usize> = lines
        .iter()
        .map(|line| wrapped_line_count(line, body.width))
        .collect();
    Some(detail_max_scroll(&rows_per_line, body.height))
}

/// The line a detail region spends on what it could not show.
///
/// Stated in rows, and on one of them at every width a supported terminal can
/// give a detail region: the phrase gives up its advice, then its words,
/// before it would wrap, because the one string whose whole job is to report
/// that content was cut must not itself be cut. Narrower than the shortest
/// form — a handful of cells, which no supported layout produces — even the
/// bare ellipsis it falls back to can wrap.
///
/// Shared by both detail regions so a reader who has learnt to look for it on
/// one screen finds the same sentence on the other. The Inventory's region
/// swaps the advice for the keys once they are live (see [`rows_below_notice`])
/// — the count, the tone, and the place it is set stay exactly where that
/// reader learnt to look.
fn dropped_rows_notice(hidden: usize, width: u16) -> Line<'static> {
    let plural = plural(hidden);
    hidden_rows_notice(
        [
            format!("{hidden} more line{plural} — widen or lengthen the terminal"),
            format!("{hidden} more line{plural}"),
            format!("+{hidden}"),
        ],
        width,
    )
}

/// How the rows below a detail region's window can be reached.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RowsBelow {
    /// The region has the keyboard: the movement keys move its window.
    UnderTheseKeys,
    /// The region is drawn beside a focused one, so the window is a region
    /// focus away from moving.
    BehindTheFocus,
    /// Nothing the reader could press from here would reach them: the region
    /// is too short to scroll, or the keyboard belongs to something — the
    /// filter's query box — that will not give it up for a movement key.
    NotFromHere,
}

/// The line a scrollable region spends on the rows below its window.
///
/// The advice names what actually reaches them from where the user is
/// standing, which is not the same sentence in each place: a hint the focused
/// region would answer to is a promise an unfocused one cannot keep, and
/// advising a bigger terminal where a keystroke would do sends the user to
/// resize a window they did not need to touch.
fn rows_below_notice(hidden: usize, width: u16, reach: RowsBelow) -> Line<'static> {
    let plural = plural(hidden);
    let advice = match reach {
        RowsBelow::UnderTheseKeys => "j/k to scroll",
        RowsBelow::BehindTheFocus => "Tab, then j/k",
        RowsBelow::NotFromHere => return dropped_rows_notice(hidden, width),
    };
    hidden_rows_notice(
        [
            format!("{hidden} more line{plural} below — {advice}"),
            format!("{hidden} more line{plural} below"),
            format!("+{hidden}"),
        ],
        width,
    )
}

/// The line a scrolled region spends on the rows above its window.
///
/// A count, and no advice: the keys that scroll back are the ones that just
/// scrolled forward, and the notice below the window already names them where
/// they are live.
fn rows_above_notice(hidden: usize, width: u16) -> Line<'static> {
    let plural = plural(hidden);
    hidden_rows_notice(
        [format!("{hidden} line{plural} above"), format!("↑{hidden}")],
        width,
    )
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// Set a hidden-row count on one line, in the longest form that fits.
fn hidden_rows_notice(forms: impl IntoIterator<Item = String>, width: u16) -> Line<'static> {
    forms
        .into_iter()
        .map(|label| Line::from(components::badge(Tone::Warning, &label)))
        .find(|line| wrapped_line_count(line, width) == 1)
        .unwrap_or_else(|| Line::from(components::badge(Tone::Warning, "…")))
}

fn inventory_detail_lines(
    row: &InventoryRow,
    roots: &[RootScan; 3],
    home: &Path,
    width: u16,
    overlay_findings: &[(AgentKind, &Finding)],
) -> Vec<Line<'static>> {
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
        verdict_tone(row.verdict()),
        row.verdict().label(),
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
            "Skilled could not read its registry in full, so it cannot say whether this came \
             from a registered source.",
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

    // Placed by what it says. A conflict and an exposure are findings, and
    // findings lead in this region for the reason the agent sections' own do:
    // the thing a reader came for must not be the thing that falls off the
    // bottom. Everything else — where OpenCode reads the name from, or that a
    // root was not read — states no finding, and follows the observations it
    // is about rather than displacing an agent section that carries one.
    let resolution_leads = matches!(
        row.opencode_resolution(),
        Some(
            OpenCodeResolution::Conflict { .. }
                | OpenCodeResolution::ForeignExposure { .. }
                | OpenCodeResolution::IncompatibleExposure { .. },
        )
    );
    if resolution_leads {
        push_opencode_resolution(&mut lines, row, roots, home, width);
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
        lines.extend(
            overlay_findings
                .iter()
                .filter(|(agent, _)| *agent == observation.agent())
                .flat_map(|(_, finding)| finding_lines(finding, width)),
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

    if !resolution_leads {
        push_opencode_resolution(&mut lines, row, roots, home, width);
    }

    // The agents that carry nothing share one line per answer rather than
    // three empty sections, so the observations that exist keep the room.
    // There are three answers, and they are kept apart: NOT INSTALLED is an
    // observation from a root that was read, NO ROOT says the root itself
    // does not exist, and NOT READ means the scan never looked and says why
    // in the scanner's own words. Folding any two together would flatten the
    // scanner's distinctions.
    let mut not_installed = Vec::new();
    let mut no_root = Vec::new();
    let mut not_read = Vec::new();
    for agent in AgentKind::ALL {
        if row.observation(agent).is_some() {
            continue;
        }
        let status = roots
            .iter()
            .find(|root| root.agent() == agent)
            .map(RootScan::status);
        match status {
            Some(RootStatus::Scanned { .. }) => {
                not_installed.push(agent.display_name().to_owned());
            }
            Some(RootStatus::Missing) => no_root.push(agent.display_name().to_owned()),
            Some(unread) => {
                not_read.push(format!(
                    "{} ({})",
                    agent.display_name(),
                    unread.short_summary()
                ));
            }
            // A root the snapshot does not list was never approached; it
            // borrows the not-scanned wording rather than coining its own.
            None => not_read.push(format!(
                "{} ({})",
                agent.display_name(),
                RootStatus::NotScanned.short_summary()
            )),
        }
    }
    if !not_installed.is_empty() {
        push_detail_section(&mut lines, "NOT INSTALLED", width);
        lines.push(Line::from(components::badge(
            Tone::Inactive,
            &not_installed.join(", "),
        )));
    }
    if !no_root.is_empty() {
        push_detail_section(&mut lines, "NO ROOT", width);
        lines.push(Line::from(components::badge(
            Tone::Inactive,
            &no_root.join(", "),
        )));
    }
    if !not_read.is_empty() {
        push_detail_section(&mut lines, "NOT READ", width);
        lines.push(Line::from(components::badge(
            Tone::Inactive,
            &not_read.join(", "),
        )));
    }
    lines
}

/// What OpenCode would load for this name, where that is not already implied
/// by the agent sections below.
///
/// A row OpenCode holds itself, aliased nowhere, is fully described by its own
/// OPENCODE section; saying so twice would spend the region's scarcest resource
/// on a restatement. Everything else — an alias, a conflict, an exposure, a
/// root that was not read, or content OpenCode reaches only through another
/// agent's root — is a fact no single section carries.
fn push_opencode_resolution(
    lines: &mut Vec<Line<'static>>,
    row: &InventoryRow,
    roots: &[RootScan; 3],
    home: &Path,
    width: u16,
) {
    let Some(resolution) = row.opencode_resolution() else {
        return;
    };
    let badge = match resolution {
        OpenCodeResolution::NothingVisible => return,
        OpenCodeResolution::Selected { winner, aliases }
            if aliases.is_empty() && winner.root() == AgentKind::OpenCode =>
        {
            return;
        }
        OpenCodeResolution::Selected { .. } => components::badge(Tone::Healthy, "resolved"),
        OpenCodeResolution::ForeignExposure { .. } => {
            components::badge(Tone::Warning, "foreign variant")
        }
        OpenCodeResolution::IncompatibleExposure { .. } => {
            components::badge(Tone::Warning, "incompatible")
        }
        OpenCodeResolution::Conflict { .. } => components::badge(Tone::Critical, "conflict"),
        // The same "could not tell" the scanner keeps everywhere else: a root
        // Skilled was asked to leave alone was not read, and a lower root can
        // hold the very directory that would make this a conflict.
        OpenCodeResolution::Incomplete { .. } => {
            components::badge(Tone::Inactive, "could not tell")
        }
    };
    push_detail_section_badge(lines, "OPENCODE RESOLUTION", badge, width);
    // Findings first, as in every agent section: the reason a resolution is
    // weakened is what this region exists to report.
    //
    // Informational ones are left to the fields below, which state the same
    // paths in the reader's own notation: a benign alias is exactly what
    // `Loads` and `Also at` already say, and repeating it as a paragraph would
    // spend four rows of the region's scarcest space on the arrangement a user
    // who installed one skill for three agents has deliberately made. Doctor
    // still lists it under its code, with its evidence whole.
    lines.extend(
        row.resolution_findings()
            .iter()
            .filter(|finding| finding.severity() > FindingSeverity::Info)
            .flat_map(|finding| finding_lines(finding, width)),
    );
    match resolution {
        OpenCodeResolution::NothingVisible => {}
        OpenCodeResolution::Selected { winner, aliases }
        | OpenCodeResolution::ForeignExposure { winner, aliases }
        | OpenCodeResolution::IncompatibleExposure { winner, aliases } => {
            lines.push(loaded_field("Loads", winner, home, width));
            if !aliases.is_empty() {
                lines.push(detail_field_bounded(
                    "Also at",
                    &joined_entry_paths(aliases, home),
                    width,
                    3,
                ));
            }
        }
        OpenCodeResolution::Conflict { entries } => {
            if let Some(winner) = entries.first() {
                lines.push(loaded_field("Would load", winner, home, width));
            }
            lines.push(detail_field_bounded(
                "Also at",
                &joined_entry_paths(entries.get(1..).unwrap_or_default(), home),
                width,
                3,
            ));
        }
        OpenCodeResolution::Incomplete { roots: unread } => {
            // Each root says why it was not read, in the scanner's own words
            // (`short_summary`, the vocabulary the setup scan step and the
            // NOT READ section use): a root left alone on the user's own
            // instruction and one that defeated the scan are not the same
            // answer, and "not read" alone would flatten them into one.
            let reasons: Vec<String> = unread
                .iter()
                .map(|unknown| {
                    let cause = match unknown.cause() {
                        // A root that was read holds one entry the scan could
                        // not follow, which is not the root being unread.
                        UnknownCause::EntryUnresolved => "entry unresolved".to_owned(),
                        UnknownCause::RootNotRead => {
                            roots[unknown.root().index()].status().short_summary()
                        }
                    };
                    format!("{} ({cause})", unknown.root().display_name())
                })
                .collect();
            lines.push(detail_field_bounded(
                "Not known",
                &reasons.join(", "),
                width,
                3,
            ));
        }
    }
}

fn loaded_field(label: &str, entry: &OpenCodeEntry, home: &Path, width: u16) -> Line<'static> {
    detail_field_bounded(label, &home_relative(entry.path(), home), width, 2)
}

fn joined_entry_paths(entries: &[OpenCodeEntry], home: &Path) -> String {
    entries
        .iter()
        .map(|entry| home_relative(entry.path(), home))
        .collect::<Vec<_>>()
        .join(", ")
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

/// More segments than this spend the pane on separators and one-cell markers;
/// large registries retain the exact textual count instead.
const UPDATE_PROGRESS_SEGMENT_BUDGET: usize = 12;

fn render_updates(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp) {
    match viewport::workspace_regions(area) {
        (primary, Some(detail)) => {
            render_update_candidates(frame, primary, app);
            render_update_details(frame, detail, app, true);
        }
        (primary, None) => match app.updates_pane() {
            UpdatesPane::Candidates => render_update_candidates(frame, primary, app),
            UpdatesPane::Details => render_update_details(frame, primary, app, false),
        },
    }
}

fn render_update_candidates(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp) {
    let subtitle = app.stated_update_count().map_or_else(
        || "network access is explicit".to_owned(),
        |count| format!("{count} available · network access is explicit"),
    );
    let body = render_pane_scaffold(
        frame,
        area,
        "Updates",
        &subtitle,
        app.updates_pane() == UpdatesPane::Candidates,
        false,
    );
    if app.sources().is_empty() {
        frame.render_widget(
            components::empty_state(
                "·",
                "No registered sources",
                "Register a local Git source before checking for repository updates.",
                body,
            ),
            body,
        );
        return;
    }
    let progress = app.update_check_progress();
    let status = if app.update_check_in_flight() {
        progress.map_or_else(
            || "Checking registered sources · Esc cancels".to_owned(),
            |(completed, total)| {
                format!(
                    "Checking source {} of {} · Esc cancels",
                    (completed + 1).min(total),
                    total
                )
            },
        )
    } else {
        "Last checked results are cached; opening this view never fetches.".to_owned()
    };
    let mut lines = vec![Line::styled(status, theme::pane_subtitle())];
    if app.update_check_in_flight()
        && let Some((completed, total)) = progress
        && total > 0
        && total <= UPDATE_PROGRESS_SEGMENT_BUDGET
        && total.saturating_mul(2).saturating_sub(1) <= usize::from(body.width)
    {
        lines.push(components::segmented_progress(
            (completed + 1).min(total),
            total,
            body.width,
        ));
    }
    if let Some(error) = app.update_check_error() {
        lines.push(Line::from(components::badge(
            Tone::Warning,
            &terminal_safe(error),
        )));
    }
    let capacity = usize::from(body.height).saturating_sub(lines.len()).max(1);
    let start = visible_window_start(app.focused_update(), capacity);
    for (index, source) in app.sources().iter().enumerate().skip(start).take(capacity) {
        let (status, tone, checked) = match app.update_check_for(source.id()) {
            None => ("not checked".to_owned(), Tone::Inactive, String::new()),
            Some(check) if check.superseded_by(source) => (
                "superseded".to_owned(),
                Tone::Warning,
                format!(" · checked {}", format_update_timestamp(check.checked_at)),
            ),
            Some(check) => {
                let status = match check.verdict {
                    RepositoryUpdateVerdict::Available => {
                        format!("available · {} behind", check.behind)
                    }
                    RepositoryUpdateVerdict::Ahead => format!("ahead · {}", check.ahead),
                    RepositoryUpdateVerdict::UpToDate => "up to date".to_owned(),
                    RepositoryUpdateVerdict::Blocked => "blocked".to_owned(),
                };
                let tone = match check.verdict {
                    RepositoryUpdateVerdict::Available => Tone::Healthy,
                    RepositoryUpdateVerdict::Blocked => Tone::Warning,
                    _ => Tone::Inactive,
                };
                (
                    status,
                    tone,
                    format!(" · checked {}", format_update_timestamp(check.checked_at)),
                )
            }
        };
        let spans = vec![
            Span::raw(format!("{:<24}", terminal_safe(source.label()))),
            components::badge(tone, &status),
            Span::styled(checked, theme::pane_subtitle()),
        ];
        lines.push(components::list_row(
            spans,
            index == app.focused_update(),
            body.width,
        ));
    }
    frame.render_widget(Paragraph::new(lines), body);
}

fn render_update_details(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp, wide: bool) {
    let body = render_pane_scaffold(
        frame,
        area,
        "Update detail",
        "cached safety state",
        app.updates_pane() == UpdatesPane::Details,
        wide,
    );
    let Some(source) = app.selected_update_source() else {
        frame.render_widget(
            components::empty_state(
                "·",
                "No source selected",
                "There is no repository update to describe.",
                body,
            ),
            body,
        );
        return;
    };
    render_detail_window(
        frame,
        body,
        update_detail_lines(app, source),
        app.detail_scroll(),
        rows_below_advice(app),
    );
}

fn update_detail_lines(app: &SkilledApp, source: &RegisteredSource) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::raw(format!("Source       {}", terminal_safe(source.label()))),
        Line::raw(format!(
            "Path         {}",
            terminal_safe(&source.git_top_level().display().to_string())
        )),
        Line::raw("Network      explicit — press u to fetch"),
    ];
    if let Some(error) = app.update_check_error() {
        lines.push(Line::raw(format!("Check error  {}", terminal_safe(error))));
    }
    match app.update_check_for(source.id()) {
        None => lines.push(Line::raw("Precondition check has not run")),
        Some(check) if check.superseded_by(source) => {
            lines.push(Line::raw("Precondition cached result is superseded"))
        }
        Some(check) => {
            lines.push(Line::raw(format!(
                "Checked      {}",
                format_update_timestamp(check.checked_at)
            )));
            lines.push(Line::raw(format!(
                "Ahead/behind {} / {}",
                check.ahead, check.behind
            )));
            for finding in check.findings() {
                lines.push(Line::raw(format!(
                    "Evidence     {} — {}",
                    finding.code(),
                    terminal_safe(finding.evidence())
                )));
            }
        }
    }
    lines
}

fn update_prompt_lines(prompt: &RepositoryUpdatePrompt) -> Vec<Line<'static>> {
    match prompt {
        RepositoryUpdatePrompt::Failed(error) => vec![
            Line::raw("Repository update could not be prepared"),
            Line::raw(terminal_safe(error)),
        ],
        RepositoryUpdatePrompt::StateUnavailable {
            apply_error,
            write_attempted,
            refresh_error,
        } => {
            let headline = if !write_attempted {
                "Repository update was abandoned without writing"
            } else if apply_error.is_some() {
                "Fast-forward command failed; post-attempt state is unavailable"
            } else {
                "Fast-forward completed; post-attempt state is unavailable"
            };
            let mut lines = vec![Line::raw(headline)];
            if let Some(error) = apply_error {
                lines.push(Line::raw(format!(
                    "{}: {}",
                    if *write_attempted {
                        "Command failure"
                    } else {
                        "Guard refusal"
                    },
                    terminal_safe(error)
                )));
            }
            lines.push(Line::raw(format!(
                "Post-attempt state unavailable: {}",
                terminal_safe(refresh_error)
            )));
            lines
        }
        RepositoryUpdatePrompt::Preview(plan) => {
            let mut lines = update_plan_statement_lines(prompt);
            for path in plan.changed_files() {
                lines.push(if let Some(old) = path.renamed_from() {
                    Line::raw(format!(
                        "  renamed · {} → {}",
                        terminal_safe(&old.display().to_string()),
                        terminal_safe(&path.path().display().to_string())
                    ))
                } else {
                    Line::raw(format!(
                        "  {:?} · {}",
                        path.kind(),
                        terminal_safe(&path.path().display().to_string())
                    ))
                });
            }
            lines
        }
        RepositoryUpdatePrompt::Report {
            verification,
            apply_error,
            write_attempted,
            persistence_error,
            ..
        } => {
            let headline = if !write_attempted {
                "Repository update was abandoned without writing"
            } else if apply_error.is_some() && verification.is_verified() {
                "Fast-forward command failed; the previewed target was nevertheless verified"
            } else if apply_error.is_some() {
                "Fast-forward failed and the post-attempt state was not verified"
            } else if verification.is_complete() && verification.is_verified() {
                "Fast-forward verified"
            } else if verification.is_verified() {
                "Fast-forward verified as far as it could be"
            } else {
                "Fast-forward was not verified"
            };
            let mut lines = vec![Line::raw(headline)];
            if let Some(error) = apply_error {
                lines.push(Line::raw(format!(
                    "{}: {}",
                    if *write_attempted {
                        "Command failure"
                    } else {
                        "Guard refusal"
                    },
                    terminal_safe(error)
                )));
            }
            lines.extend(
                verification
                    .failures()
                    .iter()
                    .map(|value| Line::raw(format!("Not verified: {}", terminal_safe(value)))),
            );
            lines.extend(
                verification
                    .withheld()
                    .iter()
                    .map(|value| Line::raw(format!("Not established: {}", terminal_safe(value)))),
            );
            if let Some(error) = persistence_error {
                lines.push(Line::raw(format!(
                    "Metadata warning: {}",
                    terminal_safe(error)
                )));
            }
            lines
        }
    }
}

fn update_plan_statement_lines(prompt: &RepositoryUpdatePrompt) -> Vec<Line<'static>> {
    let RepositoryUpdatePrompt::Preview(plan) = prompt else {
        return Vec::new();
    };
    let mut lines = vec![
        Line::raw(format!("Source: {}", terminal_safe(plan.source_label()))),
        Line::raw(format!(
            "Path: {}",
            terminal_safe(&plan.path().display().to_string())
        )),
        Line::raw(format!(
            "Branch: {}",
            terminal_safe(plan.current_reference())
        )),
        Line::raw(format!("Current: {}", plan.current_revision())),
        Line::raw(format!("Target: {}", plan.target_revision())),
        Line::raw(format!(
            "Commits: {} · changed files: {}",
            plan.commits().len(),
            plan.changed_files().len()
        )),
        Line::raw(plan.hooks_disclosure().to_owned()),
        Line::raw(plan.affected().incomplete_reason.as_deref().map_or_else(
            || "Affected installations: complete".to_owned(),
            |reason| format!("Affected installations: partial — {reason}"),
        )),
    ];
    for name in &plan.affected().updated {
        lines.push(Line::raw(format!(
            "  updated in place · {}",
            terminal_safe(name)
        )));
    }
    for name in &plan.affected().removed {
        lines.push(Line::raw(format!("  removed · {}", terminal_safe(name))));
    }
    for name in &plan.affected().added {
        lines.push(Line::raw(format!(
            "  added upstream, not installed · {}",
            terminal_safe(name)
        )));
    }
    for (installed, skill) in &plan.affected().restored {
        lines.push(Line::raw(format!(
            "  installation starts loading · {} → {}",
            terminal_safe(installed),
            terminal_safe(skill)
        )));
    }
    for (old, new, aliases) in &plan.affected().renamed {
        lines.push(Line::raw(format!(
            "  renamed · {} → {}",
            terminal_safe(old),
            terminal_safe(new)
        )));
        // A link installed under a name of its own is not named by the pair
        // above, and naming it is not enough either: what the rename does to it
        // is leave it with nothing to resolve to, and that is the outcome
        // verification will hold this update to.
        for alias in aliases {
            lines.push(Line::raw(format!(
                "    loses its target · {}",
                terminal_safe(alias)
            )));
        }
    }
    for finding in plan.findings() {
        lines.push(Line::raw(format!(
            "Blocked: {} — {}",
            finding.code(),
            terminal_safe(finding.evidence())
        )));
    }
    // The commits are what the fast-forward brings in, so they are part of what
    // is being agreed to rather than evidence under it: the gate measures these
    // rows too, and Enter stays unavailable until the last of them has been on
    // screen. Only the changed-file listing below is non-gating.
    for commit in plan.commits() {
        lines.push(Line::raw(format!("  commit · {}", terminal_safe(commit))));
    }
    lines
}

fn update_prompt_regions(area: Rect, action_width: u16) -> components::DialogRegions {
    let popup = install_prompt_popup(area);
    let block = components::dialog_frame("Repository update", "fast-forward only");
    components::dialog_regions(block.inner(popup), action_width)
}

fn update_preview_fully_seen(app: &SkilledApp, area: Rect) -> Option<bool> {
    let prompt = app.pending_update()?;
    if !matches!(prompt, RepositoryUpdatePrompt::Preview(_)) {
        return None;
    }
    let body = update_prompt_regions(area, 0).body;
    if body.width == 0 || body.height == 0 {
        return None;
    }
    let rows = visual_rows(update_plan_statement_lines(prompt), body.width).len();
    let required = rows.saturating_sub(usize::from(body.height));
    Some(app.detail_scroll() >= required)
}

fn render_update_prompt(
    frame: &mut Frame<'_>,
    area: Rect,
    prompt: &RepositoryUpdatePrompt,
    scroll: usize,
    extent: Option<usize>,
    fully_seen: bool,
) {
    let popup = install_prompt_popup(area);
    frame.render_widget(Clear, popup);
    let block = components::dialog_frame("Repository update", "fast-forward only");
    let hint = match prompt {
        RepositoryUpdatePrompt::Preview(plan) if !plan.is_blocked() && fully_seen => {
            "Enter Apply · Esc Cancel"
        }
        RepositoryUpdatePrompt::Preview(_) => "j/k Read plan · Esc Cancel",
        _ => "Esc Close",
    };
    let regions = update_prompt_regions(
        area,
        u16::try_from(hint.chars().count()).unwrap_or(u16::MAX),
    );
    frame.render_widget(block, popup);
    let rows = update_prompt_rows(prompt, regions.body.width);
    let end = scroll
        .saturating_add(usize::from(regions.body.height))
        .min(rows.len());
    let visible = rows.get(scroll.min(rows.len())..end).unwrap_or_default();
    frame.render_widget(Paragraph::new(visible.to_vec()), regions.body);
    frame.render_widget(
        Paragraph::new(components::rule(regions.divider.width)),
        regions.divider,
    );
    // The commit summaries gate the confirmation, so what is still below the
    // viewport is not always evidence. Naming it evidence while gating rows
    // are unread would say the plan has been shown when Enter is still
    // withheld — the opposite of what the gate is for.
    let status = match extent {
        Some(max) if scroll < max && !fully_seen => "Plan continues below",
        Some(max) if scroll < max => "Changed-file evidence continues below",
        Some(max) if max > 0 => "Changed-file evidence ends here",
        _ => "Complete plan and evidence shown",
    };
    frame.render_widget(Paragraph::new(status), regions.status);
    frame.render_widget(Paragraph::new(hint).right_aligned(), regions.actions);
}

fn update_prompt_rows(prompt: &RepositoryUpdatePrompt, width: u16) -> Vec<Line<'static>> {
    visual_rows(update_prompt_lines(prompt), width)
}

/// Materialize the update dialog as terminal rows. Unlike `Paragraph::scroll`,
/// this keeps the logical offset as `usize`, so complete evidence remains
/// reachable after row 65,535 instead of wrapping through a `u16` cast.
fn visual_rows(lines: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let width = usize::from(width);
    let mut rows = Vec::new();
    for line in lines {
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        if text.is_empty() {
            rows.push(Line::raw(String::new()));
            continue;
        }
        let mut row = String::new();
        let mut row_width = 0_usize;
        for character in text.chars() {
            let character_width = Span::raw(character.to_string()).width();
            if !row.is_empty() && row_width.saturating_add(character_width) > width {
                rows.push(Line::raw(std::mem::take(&mut row)));
                row_width = 0;
            }
            row.push(character);
            row_width = row_width.saturating_add(character_width);
        }
        rows.push(Line::raw(row));
    }
    rows
}

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
    let metadata_unavailable = app.registry_availability() == RegistryAvailability::Unavailable;
    let subtitle = if metadata_unavailable {
        "registry unavailable".to_owned()
    } else {
        format!("{} registered", app.sources().len())
    };
    let inner = render_pane_scaffold(
        frame,
        area,
        "Repositories",
        &subtitle,
        app.sources_pane() == SourcesPane::Repositories,
        false,
    );

    if app.sources().is_empty() {
        // Whether the registry could be read and whether a source may be added
        // are separate questions, and the empty state has to answer both. A
        // readable registry still states its zero, but any degraded metadata
        // unit refuses the key, so naming `a` here would advertise something
        // `input` deliberately filters.
        let (headline, explanation) = match (metadata_unavailable, app.can_add_source()) {
            (true, _) => (
                "Source registry is unavailable",
                "Skilled cannot state which sources are registered, and adding one is disabled \
                 for this session.",
            ),
            (false, false) => (
                "No sources are registered",
                "Adding one is disabled for this session because Skilled could not read its own \
                 metadata.",
            ),
            (false, true) => (
                "No sources are registered",
                "Press a to register a local Git checkout.",
            ),
        };
        frame.render_widget(
            components::empty_state("·", headline, explanation, inner),
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
        false,
    );

    let Some(source) = app.selected_source() else {
        // The same two questions the Repositories pane beside this one asks,
        // answered the same way: whether the registry could be read decides
        // what may be claimed about it, and whether metadata is writable
        // decides whether the key may be named. Reading the session-wide
        // failure for both would call a registry unavailable that the pane
        // beside it has just counted.
        let explanation = match (
            app.registry_availability() == RegistryAvailability::Unavailable,
            app.can_add_source(),
        ) {
            (true, _) => "The source registry is unavailable in this session.",
            (false, false) => {
                "Adding one is disabled for this session because Skilled could not read its own \
                 metadata."
            }
            (false, true) => "Press a to register a local Git checkout.",
        };
        frame.render_widget(
            components::empty_state("·", "No source selected", explanation, inner),
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
    // group labels sit between the rows and only this loop knows where they
    // fell.
    //
    // Which rows exist, and in what order, is `catalog_rows`' to say — the
    // same sequence `variants_row_count` counts and `selected_variant_row`
    // indexes into, so the window follows the selection and a list taller than
    // the pane can be walked whatever mixture of skills, errors, and empty
    // catalogs it holds. This loop only draws them.
    let mut lines = Vec::new();
    let mut group_labels = Vec::new();
    let mut focused_line = 0;
    let mut position = 0;
    for catalog in source.catalogs() {
        let label = lines.len();
        lines.push(catalog_group_label(catalog, inner.width, beside_details));
        group_labels.push(label);
        for row in catalog_rows(catalog) {
            let selected = position == app.focused_variant();
            if selected {
                focused_line = lines.len();
            }
            lines.push(variants_pane_row(row, selected, inner.width));
            group_labels.push(label);
            position += 1;
        }
    }
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

/// One row of the variants pane, drawn according to its kind.
fn variants_pane_row(row: SourceRow<'_>, selected: bool, width: u16) -> Line<'static> {
    match row {
        SourceRow::CatalogError { error, .. } => {
            let badge = components::badge(Tone::Critical, "unavailable");
            // Bounded to the pane like every other row: a wrapped error would
            // put the marker and the band on one line and the words on the
            // next. The detail region gives the message more room — three
            // bounded lines — but a message past those is elided there too.
            let budget = usize::from(width)
                .min(VARIANTS_CONTENT_MAX_WIDTH)
                .saturating_sub(ROW_MARKER_WIDTH + badge.width() + 1);
            components::list_row(
                vec![
                    badge,
                    Span::raw(format!(" {}", terminal_safe_bounded_start(error, budget))),
                ],
                selected,
                width,
            )
        }
        SourceRow::Variant { candidate, .. } => variant_row(candidate, selected, width),
        // Said rather than left blank: two labels in a row would read as
        // though the rows under the first belonged to both, and a label with
        // nothing under it would not say whether the catalog is empty or the
        // list has scrolled.
        SourceRow::NoVariants(_) => components::list_row(
            vec![Span::styled(
                "no variants".to_owned(),
                theme::pane_subtitle(),
            )],
            selected,
            width,
        ),
    }
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
        &registration_claim(catalog.compatibility()),
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

/// What the detail region calls the set of agents a catalog is registered for.
///
/// The label is Skilled's own. The prototype holds this as a `compatibility`
/// key and renders it only as a bare qualifier in the catalog title — which is
/// where [`catalog_group_label`] still renders it, wording untouched — and its
/// source detail has no such field at all. So the departure recorded here is
/// not from the prototype's label but from Skilled's earlier one, which called
/// the field `Compatibility`.
///
/// Nothing here is a compatibility statement: the value is the registration
/// Skilled proposed and the user confirmed, and no skill was inspected for
/// what it can run under and no agent was asked. `Compatibility: Claude Code`
/// would read as a finding about the catalog; `Registered for: Claude Code`
/// says who it was filed under, which is the fact that exists.
///
/// The inventory's `Registered for some agents but not others` speaks of a
/// different subject — an installed skill that resolved to a registered source
/// in some agent roots and not others. Both are true uses of the word: this
/// field says what a catalog was filed under, that line says where an object
/// came from. They are never on screen together, and each states its subject.
const REGISTRATION_LABEL: &str = "Registered for";

/// How many lines the registration claim may occupy in the detail region.
///
/// Two, not one. The label is a column longer than the one it replaces, and
/// the longest claim — two agents named in full — then outruns the narrowest
/// region's line by exactly that column. Given one line it would be elided,
/// and an elided claim names agents that were never registered, which is the
/// one thing this field must not do. So it wraps: the claim stands whole and
/// spends a second row only in the narrowest region and only where a shorter
/// claim would not have needed it.
const REGISTRATION_CLAIM_LINES: usize = 2;

/// Which agents a catalog is registered for, for the variants group label and
/// the detail region's CATALOG section alike.
///
/// Skilled proposes this from the catalog's place in the checkout and the user
/// confirms or edits it; the catalog itself declares nothing and no agent was
/// asked. So the phrase names what is stored and nothing more. A catalog
/// registered for none says so rather than rendering an empty phrase, and one
/// registered for some names those and stops: the agents left out are the ones
/// not claimed, which is what the setup dialog's exhaustive yes/no list is for.
fn registration_claim(compatibility: Compatibility) -> String {
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

fn format_update_timestamp(generation: i64) -> String {
    format_scan_timestamp(generation.div_euclid(1_000_000_000))
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
        .map(|variant| terminal_safe(variant.directory_name()))
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
        false,
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
            REGISTRATION_LABEL,
            &registration_claim(catalog.compatibility()),
            inner.width,
            REGISTRATION_CLAIM_LINES,
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
        let (status, name) = match variant.validation() {
            SkillValidation::Valid { name, .. } => {
                (components::badge(Tone::Healthy, "valid"), name.as_str())
            }
            SkillValidation::Invalid { .. } => (
                components::badge(Tone::Critical, "invalid"),
                variant.directory_name(),
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
                terminal_safe(variant.directory_name()),
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
            &variant.relative_path().display().to_string(),
            inner.width,
            1,
        ));
        variant_lines.push(Line::from(vec![
            Span::styled("Status: ", theme::pane_subtitle()),
            status,
        ]));
        let variant_essential_height = detail_lines_height(&variant_lines, inner.width);
        match variant.validation() {
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

/// Lay the three sections into the region, saying so when they do not fit.
///
/// The sections are budgeted against each other — the variant and catalog
/// essentials are reserved first, and the repository section gives up its
/// lines before either — but whichever section ends up short, what it drops
/// falls off the bottom of the region unremarked. So the region reports it,
/// the way the inventory's does: the last row is spent on a count of the rows
/// none of the three could show, because a region that ends mid-sentence reads
/// as though the sentence had ended.
///
/// One notice for the region rather than one per section: three apologies
/// would cost three rows of the content they are apologising for, and the
/// reader's question is what this screen is not telling them, not which of its
/// headings the answer sat under.
fn render_detail_regions(
    frame: &mut Frame<'_>,
    area: Rect,
    repository_lines: Vec<Line<'static>>,
    catalog_lines: Vec<Line<'static>>,
    catalog_essential_height: usize,
    variant_lines: Vec<Line<'static>>,
    variant_essential_height: usize,
) {
    let section_rows = [&repository_lines, &catalog_lines, &variant_lines]
        .map(|lines| detail_lines_height(lines, area.width));
    // Measured before the sections are budgeted, because the row the notice
    // takes is a row they cannot have. It is measured against the total, which
    // stands in for the count only known once they have been laid out — and
    // that count can only be smaller, so the notice finally rendered is never
    // wider, and never needs a row this reservation did not buy it.
    let total_rows = section_rows.iter().sum::<usize>();
    let notice_rows = if total_rows > usize::from(area.height) {
        wrapped_line_count(&dropped_rows_notice(total_rows, area.width), area.width)
    } else {
        0
    };
    let [content, notice_area] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(u16::try_from(notice_rows).unwrap_or(u16::MAX)),
    ])
    .areas(area);

    let layout = detail_region_layout(
        section_rows,
        [catalog_essential_height, variant_essential_height],
        usize::from(content.height),
    );
    let [repository_area, catalog_area, variant_area] = Layout::vertical(
        layout
            .heights
            .map(|height| Constraint::Length(u16::try_from(height).unwrap_or(u16::MAX))),
    )
    .areas(content);
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
    if notice_rows > 0 {
        frame.render_widget(
            Paragraph::new(dropped_rows_notice(layout.hidden, content.width))
                .wrap(Wrap { trim: false }),
            notice_area,
        );
    }
}

/// How the three detail sections divide a region, and what that leaves unsaid.
struct DetailRegionLayout {
    /// Rows given to the repository, catalog, and variant sections in order.
    heights: [usize; 3],
    /// Rows of content none of them had room for.
    hidden: usize,
}

/// Divide `available` rows between the three sections, reporting what did not
/// fit.
///
/// The variant and catalog essentials are reserved first and the repository
/// section gives up its rows before either, because the sections below it are
/// the ones the selection just moved to. Every row of the region is handed to
/// some section — the last one takes whatever the first two left — so what is
/// hidden is what the sections wanted beyond the region, and no arithmetic
/// over the rendered widgets is needed to find it.
///
/// An essential taller than the section that asked for it is clamped to it: a
/// section cannot be promised more rows than it has content to put in them,
/// and a reservation held open for rows that do not exist would push another
/// section's content off the region while this count, which measures the rows
/// the sections were given, saw nothing missing. Today's callers derive both
/// essentials from the same lines at the same width and so never exceed them,
/// but the count is the one thing here that must not depend on a caller
/// getting that right.
///
/// Split out from the rendering so the count the region states can be checked
/// against every shape of content, rather than only against the shapes a
/// fixture happens to produce: a notice that misreports is worse than no
/// notice, because it is read as a measurement.
fn detail_region_layout(
    section_rows: [usize; 3],
    essential_heights: [usize; 2],
    available: usize,
) -> DetailRegionLayout {
    let [repository_rows, catalog_rows, variant_rows] = section_rows;
    let [catalog_essential, variant_essential] = essential_heights;
    let catalog_essential = catalog_essential.min(catalog_rows);
    let variant_essential = variant_essential.min(variant_rows);
    let reserved_variant = variant_essential.min(available);
    let reserved_catalog = catalog_essential.min(available.saturating_sub(reserved_variant));
    let repository_height = repository_rows.min(
        available
            .saturating_sub(reserved_catalog)
            .saturating_sub(reserved_variant),
    );
    let after_repository = available.saturating_sub(repository_height);
    let catalog_height = catalog_rows.min(after_repository.saturating_sub(reserved_variant));
    let variant_height = after_repository.saturating_sub(catalog_height);
    DetailRegionLayout {
        heights: [repository_height, catalog_height, variant_height],
        hidden: (repository_rows + catalog_rows + variant_rows)
            .saturating_sub(repository_height + catalog_height + variant_height),
    }
}

fn detail_lines_height(lines: &[Line<'_>], width: u16) -> usize {
    lines
        .iter()
        .map(|line| wrapped_line_count(line, width))
        .sum()
}

/// Every candidate the source holds, for the pane subtitle's count.
///
/// A flat tally of skills, not of rows: the subtitle says how many variants
/// were found, which is not what the selection walks.
fn flattened_variants(source: &RegisteredSource) -> Vec<&SkillCandidate> {
    source
        .catalogs()
        .iter()
        .flat_map(CatalogProposal::candidates)
        .collect()
}

/// The variant the selection rests on, or `None` where it rests on a
/// catalog's state row — which names no variant, and whose detail region says
/// so rather than describing the last one that happened to be selected.
fn selected_variant(app: &SkilledApp) -> Option<&SkillCandidate> {
    match app.selected_variant_row()? {
        SourceRow::Variant { candidate, .. } => Some(candidate),
        SourceRow::CatalogError { .. } | SourceRow::NoVariants(_) => None,
    }
}

/// The catalog the selection rests in, whichever row kind carries it: a
/// selected state row names its catalog as surely as a selected variant does,
/// so the Details CATALOG section follows the band across the region
/// boundary instead of rendering identically for every position.
fn selected_catalog(app: &SkilledApp) -> Option<&CatalogProposal> {
    Some(app.selected_variant_row()?.catalog())
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

/// Where the install dialog sits, and how its interior divides.
///
/// The body is measured and drawn through one function so the extent the frame
/// reports and the window it drew cannot disagree: a body measured against a
/// different height from the one rendered would clamp the offset to the wrong
/// end. `action_width` divides the footer alone and leaves the body untouched,
/// so the measurement pass may pass anything for it.
fn install_prompt_regions(area: Rect, action_width: u16) -> components::DialogRegions {
    let popup = install_prompt_popup(area);
    let block = components::dialog_frame("Install skill", "nothing written yet");
    components::dialog_regions(block.inner(popup), action_width)
}

/// The rectangle the dialog occupies, computed once for the frame that draws it
/// and the pass that measures it.
fn install_prompt_popup(area: Rect) -> Rect {
    centered_rect(
        area.width.saturating_sub(4),
        area.height.saturating_sub(2),
        area,
    )
}

/// The install dialog: what would happen, or what did.
///
/// Sized to fill the workspace rather than to a fixed shape, because its body
/// states absolute paths in full. Spec 15 asks the preview to say exactly what
/// is about to be written, and the `~` abbreviation every other screen uses to
/// speak about a global root would soften precisely the thing the user is being
/// asked to agree to. Long paths wrap and a body taller than the dialog
/// scrolls; nothing is elided and nothing is dropped.
fn render_install_prompt(
    frame: &mut Frame<'_>,
    area: Rect,
    prompt: &InstallPrompt,
    home: &Path,
    scroll: usize,
    extent: Option<usize>,
    fully_seen: bool,
) {
    let popup = install_prompt_popup(area);
    frame.render_widget(Clear, popup);
    let (title, scope) = match prompt {
        InstallPrompt::Preview(_) | InstallPrompt::Failed(_) => {
            ("Install skill", "nothing written yet")
        }
        InstallPrompt::Report(_) => ("Install result", "already applied"),
    };
    let actions = install_prompt_actions(prompt, fully_seen);
    // The footer is divided by the keys actually offered, so the sentence
    // beside them — which is where a reader is told the body holds more than
    // it can show — keeps every column the keys do not need.
    let regions = install_prompt_regions(area, u16::try_from(actions.width()).unwrap_or(u16::MAX));
    let block = components::dialog_frame(title, scope);
    frame.render_widget(block, popup);

    let lines = install_prompt_lines(prompt, home, regions.body.width);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
        regions.body,
    );
    frame.render_widget(
        Paragraph::new(components::rule(regions.divider.width)),
        regions.divider,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            install_prompt_status(prompt, scroll, extent),
            theme::key_label(),
        ))),
        regions.status,
    );
    frame.render_widget(Paragraph::new(actions.right_aligned()), regions.actions);
}

fn render_uninstall_prompt(
    frame: &mut Frame<'_>,
    area: Rect,
    prompt: &UninstallPrompt,
    scroll: usize,
    extent: Option<usize>,
    fully_seen: bool,
) {
    let popup = install_prompt_popup(area);
    frame.render_widget(Clear, popup);
    let (title, scope) = match prompt {
        UninstallPrompt::Preview(_) | UninstallPrompt::Failed(_) => {
            ("Uninstall skill", "managed links only")
        }
        UninstallPrompt::Report(_) => ("Uninstall result", "already applied"),
    };
    let confirm =
        fully_seen && matches!(prompt, UninstallPrompt::Preview(plan) if plan.is_executable());
    let mut spans = Vec::new();
    if confirm {
        spans.extend([
            Span::styled("Enter", theme::key_cap()),
            Span::raw(" "),
            Span::styled("Uninstall", theme::key_label()),
            Span::raw("   "),
        ]);
    }
    spans.extend([
        Span::styled("Esc", theme::key_cap()),
        Span::raw(" "),
        Span::styled(if confirm { "Cancel" } else { "Close" }, theme::key_label()),
    ]);
    let actions = Line::from(spans);
    let regions = install_prompt_regions(area, u16::try_from(actions.width()).unwrap_or(u16::MAX));
    frame.render_widget(components::dialog_frame(title, scope), popup);
    frame.render_widget(
        Paragraph::new(uninstall_prompt_lines(prompt))
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
        regions.body,
    );
    frame.render_widget(
        Paragraph::new(components::rule(regions.divider.width)),
        regions.divider,
    );
    let mut status = uninstall_prompt_verdict(prompt);
    if let Some(extent) = extent.filter(|extent| *extent > 0) {
        let where_to = match (scroll > 0, scroll < extent) {
            (true, true) => "more above and below",
            (true, false) => "more above",
            _ => "more below",
        };
        status.push_str(" · ");
        status.push_str(where_to);
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(status, theme::key_label()))),
        regions.status,
    );
    frame.render_widget(Paragraph::new(actions.right_aligned()), regions.actions);
}

fn uninstall_prompt_verdict(prompt: &UninstallPrompt) -> String {
    match prompt {
        UninstallPrompt::Preview(plan) if plan.is_blocked() => {
            "Blocked — nothing will be removed".to_owned()
        }
        UninstallPrompt::Preview(plan) if plan.is_executable() => {
            let count = plan
                .targets()
                .iter()
                .filter(|target| target.is_work())
                .count();
            format!(
                "{count} managed link{} to remove",
                if count == 1 { "" } else { "s" }
            )
        }
        UninstallPrompt::Preview(_) => "Nothing left to do".to_owned(),
        UninstallPrompt::Report(outcome) => match outcome.status() {
            UninstallStatus::Uninstalled if outcome.verification().is_complete() => {
                "Uninstalled and verified".to_owned()
            }
            UninstallStatus::Uninstalled => "Uninstalled · not fully verified".to_owned(),
            UninstallStatus::NothingToDo | UninstallStatus::NotApplied => {
                "Nothing was removed".to_owned()
            }
            UninstallStatus::PartiallyApplied => "Partly applied".to_owned(),
            UninstallStatus::VerificationFailed => "Removed, but not verified".to_owned(),
            UninstallStatus::UninstalledUnrecorded => {
                "Removed, but ownership metadata remains".to_owned()
            }
        },
        UninstallPrompt::Failed(_) => "No plan was made".to_owned(),
    }
}

fn uninstall_prompt_lines(prompt: &UninstallPrompt) -> Vec<Line<'static>> {
    match prompt {
        UninstallPrompt::Failed(message) => vec![Line::from(components::badge(
            Tone::Critical,
            &terminal_safe(message),
        ))],
        UninstallPrompt::Preview(plan) => {
            let blocked = plan.is_blocked();
            let mut lines = vec![
                Line::styled(
                    format!("Skill: {}", terminal_safe(plan.skill_name())),
                    theme::section_title(),
                ),
                Line::default(),
                Line::styled("Targets", theme::section_title()),
            ];
            for target in plan.targets() {
                let (tone, verdict, evidence) = match target.disposition() {
                    UninstallDisposition::RemoveLink {
                        link_target,
                        target_state,
                        receipts,
                    } => (
                        if blocked {
                            Tone::Unmanaged
                        } else {
                            Tone::Warning
                        },
                        if blocked {
                            "would remove the managed link"
                        } else {
                            "remove the managed link"
                        }
                        .to_owned(),
                        {
                            let mut evidence = vec![format!(
                                "receipt target: {}{}",
                                terminal_safe(&link_target.display().to_string()),
                                uninstall_target_suffix(target_state)
                            )];
                            evidence.extend(receipts.iter().map(|receipt| {
                                format!(
                                    "receipt source {} · catalog {} · variant {}",
                                    receipt
                                        .source_id()
                                        .map_or_else(|| "unknown".to_owned(), |id| id.to_string()),
                                    receipt.catalog_relative_path().map_or_else(
                                        || "unknown".to_owned(),
                                        |path| terminal_safe(&path.display().to_string())
                                    ),
                                    receipt.variant_relative_path().map_or_else(
                                        || "unknown".to_owned(),
                                        |path| terminal_safe(&path.display().to_string())
                                    ),
                                )
                            }));
                            evidence
                        },
                    ),
                    UninstallDisposition::Excluded { reason } => (
                        Tone::Unmanaged,
                        format!("excluded: {:?}", reason),
                        Vec::new(),
                    ),
                    UninstallDisposition::Blocked { finding } => (
                        Tone::Critical,
                        format!("blocked: {}", finding.code()),
                        vec![terminal_safe(finding.evidence())],
                    ),
                };
                lines.push(Line::from(components::badge(
                    tone,
                    &format!("{} · {verdict}", target.agent().display_name()),
                )));
                lines.push(Line::from(format!(
                    "  {}",
                    terminal_safe(&target.link_path().display().to_string())
                )));
                for evidence in evidence {
                    lines.push(Line::from(format!("  {evidence}")));
                }
            }
            if !plan.warnings().is_empty() {
                lines.push(Line::default());
                lines.push(Line::styled("Before you confirm", theme::section_title()));
                for warning in plan.warnings() {
                    lines.push(Line::from(components::badge(
                        Tone::Warning,
                        &terminal_safe(warning),
                    )));
                }
            }
            lines.push(Line::default());
            lines.push(Line::styled(
                "Source content and agent skill roots are not removed.",
                theme::key_label(),
            ));
            lines
        }
        UninstallPrompt::Report(outcome) => {
            let mut lines = vec![Line::styled(
                format!("Skill: {}", terminal_safe(outcome.plan().skill_name())),
                theme::section_title(),
            )];
            for step in outcome.applied().steps() {
                let (tone, verdict) = uninstall_step_verdict(step.outcome());
                lines.push(Line::from(components::badge(
                    tone,
                    &format!("{} · {verdict}", step.agent().display_name()),
                )));
                lines.push(Line::from(format!(
                    "  {}",
                    terminal_safe(&step.link_path().display().to_string())
                )));
            }
            for withheld in outcome.verification().withheld() {
                lines.push(Line::from(components::badge(
                    Tone::Warning,
                    &format!(
                        "Not established: {} — {}",
                        withheld.agent().display_name(),
                        terminal_safe(withheld.reason())
                    ),
                )));
            }
            for failure in outcome.verification().failures() {
                lines.push(Line::from(components::badge(
                    Tone::Critical,
                    &format!(
                        "Not verified: {} — {}",
                        failure.agent().display_name(),
                        terminal_safe(failure.observed())
                    ),
                )));
            }
            for failure in outcome.finalized().failures() {
                lines.push(Line::from(components::badge(
                    Tone::Warning,
                    &format!(
                        "Ownership record remains for {} — {}",
                        failure.agent().display_name(),
                        terminal_safe(failure.reason())
                    ),
                )));
            }
            lines.push(Line::default());
            lines.push(Line::styled(
                "Source content and agent skill roots were not removed.",
                theme::key_label(),
            ));
            lines
        }
    }
}

fn uninstall_target_suffix(state: &crate::operations::UninstallTargetState) -> &'static str {
    use crate::operations::UninstallTargetState;
    match state {
        UninstallTargetState::Directory => "",
        UninstallTargetState::Missing => " (no longer resolves)",
        UninstallTargetState::NotADirectory => " (no longer a directory)",
        UninstallTargetState::Unreadable(_) => " (could not be read)",
    }
}

fn render_forget_prompt(
    frame: &mut Frame<'_>,
    area: Rect,
    prompt: &ForgetPrompt,
    scroll: usize,
    extent: Option<usize>,
    fully_seen: bool,
) {
    let popup = install_prompt_popup(area);
    frame.render_widget(Clear, popup);
    let report = matches!(prompt, ForgetPrompt::Report(_));
    let confirm =
        fully_seen && matches!(prompt, ForgetPrompt::Preview(plan) if plan.is_executable());
    let mut spans = Vec::new();
    if confirm {
        spans.extend([
            Span::styled("Enter", theme::key_cap()),
            Span::raw(" "),
            Span::styled("Forget", theme::key_label()),
            Span::raw("   "),
        ]);
    }
    spans.extend([
        Span::styled("Esc", theme::key_cap()),
        Span::raw(" "),
        Span::styled(if confirm { "Cancel" } else { "Close" }, theme::key_label()),
    ]);
    let actions = Line::from(spans);
    let regions = install_prompt_regions(area, u16::try_from(actions.width()).unwrap_or(u16::MAX));
    frame.render_widget(
        components::dialog_frame(
            if report {
                "Forget result"
            } else {
                "Forget source"
            },
            if report {
                "already applied"
            } else {
                "metadata only"
            },
        ),
        popup,
    );
    frame.render_widget(
        Paragraph::new(forget_prompt_lines(prompt))
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
        regions.body,
    );
    frame.render_widget(
        Paragraph::new(components::rule(regions.divider.width)),
        regions.divider,
    );
    let mut status = match prompt {
        ForgetPrompt::Preview(plan) if plan.is_blocked() => {
            "Blocked — no metadata will be removed".to_owned()
        }
        ForgetPrompt::Preview(_) => "Ready to forget source metadata".to_owned(),
        ForgetPrompt::Report(outcome) => match outcome.status() {
            ForgetStatus::Forgotten => "Source forgotten and verified".to_owned(),
            ForgetStatus::NothingToDo => "Nothing was removed".to_owned(),
            ForgetStatus::NotForgotten => "Source was not forgotten".to_owned(),
            ForgetStatus::VerificationFailed => "Forgotten, but not verified".to_owned(),
        },
        ForgetPrompt::Failed(_) => "No plan was made".to_owned(),
    };
    if let Some(extent) = extent.filter(|extent| *extent > 0) {
        status.push_str(if scroll < extent {
            " · more below"
        } else {
            " · more above"
        });
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(status, theme::key_label()))),
        regions.status,
    );
    frame.render_widget(Paragraph::new(actions.right_aligned()), regions.actions);
}

fn forget_prompt_lines(prompt: &ForgetPrompt) -> Vec<Line<'static>> {
    match prompt {
        ForgetPrompt::Failed(message) => vec![Line::from(components::badge(
            Tone::Critical,
            &terminal_safe(message),
        ))],
        ForgetPrompt::Preview(plan) => {
            let source = plan.source();
            let mut lines = vec![
                Line::styled(
                    format!("Source: {}", terminal_safe(source.label())),
                    theme::section_title(),
                ),
                Line::from(format!(
                    "Checkout left untouched: {}",
                    terminal_safe(&source.git_top_level().display().to_string())
                )),
                Line::default(),
                Line::styled("Metadata to remove", theme::section_title()),
                Line::from("source registration and cached scan state"),
            ];
            for catalog in source.catalogs() {
                lines.push(Line::from(format!(
                    "catalog: {}",
                    terminal_safe(&catalog.relative_path().display().to_string())
                )));
            }
            for item in plan.receipts() {
                let receipt = item.receipt();
                match item.state() {
                    ForgetReceiptState::Active => lines.push(Line::from(components::badge(
                        Tone::Critical,
                        &format!(
                            "active link blocks: {}",
                            terminal_safe(&receipt.link_path().display().to_string())
                        ),
                    ))),
                    ForgetReceiptState::Inactive { reason } => lines.push(Line::from(format!(
                        "inactive receipt: {} — {}",
                        terminal_safe(&receipt.link_path().display().to_string()),
                        terminal_safe(reason)
                    ))),
                    ForgetReceiptState::Unreadable { reason } => {
                        lines.push(Line::from(components::badge(
                            Tone::Critical,
                            &format!(
                                "unreadable receipt: {} — {}",
                                terminal_safe(&receipt.link_path().display().to_string()),
                                terminal_safe(reason)
                            ),
                        )))
                    }
                }
            }
            if plan.receipts().is_empty() {
                for finding in plan.blocking_findings() {
                    lines.push(Line::from(components::badge(
                        Tone::Critical,
                        &format!(
                            "blocked: {} — {}",
                            finding.code(),
                            terminal_safe(finding.evidence())
                        ),
                    )));
                }
            }
            lines.push(Line::default());
            lines.push(Line::styled(
                "Skilled writes nothing to the checkout or any skill directory.",
                theme::key_label(),
            ));
            lines
        }
        ForgetPrompt::Report(outcome) => {
            let mut lines = vec![Line::styled(
                format!("Source: {}", terminal_safe(outcome.plan().source().label())),
                theme::section_title(),
            )];
            match outcome.applied() {
                ForgetApply::Forgotten => lines.push(Line::from(components::badge(
                    Tone::Healthy,
                    "private metadata removed",
                ))),
                ForgetApply::NothingToDo => {
                    lines.push(Line::from("the source row was already absent"))
                }
                ForgetApply::Failed(reason) => lines.push(Line::from(components::badge(
                    Tone::Critical,
                    &format!("not forgotten — {}", terminal_safe(reason)),
                ))),
            }
            match outcome.verification() {
                ForgetVerification::Held => lines.push(Line::from(
                    "Verified: source, catalogs, and receipts are absent; the registered \
                     checkout is still there.",
                )),
                ForgetVerification::Failed(reason) => lines.push(Line::from(components::badge(
                    Tone::Critical,
                    &terminal_safe(reason),
                ))),
                ForgetVerification::Withheld(reason) => lines.push(Line::from(components::badge(
                    Tone::Warning,
                    &terminal_safe(reason),
                ))),
            }
            lines.push(Line::default());
            lines.push(Line::styled(
                "Skilled wrote nothing to the checkout or any skill directory.",
                theme::key_label(),
            ));
            lines
        }
    }
}

/// The keys the dialog offers, which are exactly the ones the reducer honours
/// in this state: a plan with no executable work accepts no confirmation, and
/// neither does one whose last row has not been on screen, so neither
/// advertises one.
fn install_prompt_actions(prompt: &InstallPrompt, fully_seen: bool) -> Line<'static> {
    let confirm =
        fully_seen && matches!(prompt, InstallPrompt::Preview(plan) if plan.is_executable());
    let mut spans = Vec::new();
    if confirm {
        spans.extend([
            Span::styled("Enter", theme::key_cap()),
            Span::raw(" "),
            Span::styled("Install", theme::key_label()),
            Span::raw("   "),
        ]);
    }
    spans.extend([
        Span::styled("Esc", theme::key_cap()),
        Span::raw(" "),
        Span::styled(if confirm { "Cancel" } else { "Close" }, theme::key_label()),
    ]);
    Line::from(spans)
}

/// The one sentence under the rule, and — where the body does not hold
/// everything — where the rest of it is.
///
/// A preview a reader has not seen all of is not a preview they can consent to,
/// so the dialog says which way the rest lies rather than letting the paragraph
/// end without saying it did.
fn install_prompt_status(prompt: &InstallPrompt, scroll: usize, extent: Option<usize>) -> String {
    let verdict = install_prompt_verdict(prompt);
    match extent {
        Some(extent) if extent > 0 => {
            let above = scroll > 0;
            let below = scroll < extent;
            let where_to = match (above, below) {
                (true, true) => "more above and below",
                (true, false) => "more above",
                _ => "more below",
            };
            format!("{verdict} · {where_to}")
        }
        _ => verdict,
    }
}

fn install_prompt_verdict(prompt: &InstallPrompt) -> String {
    match prompt {
        InstallPrompt::Preview(plan) if plan.is_blocked() => {
            "Blocked — nothing will be written".to_owned()
        }
        InstallPrompt::Preview(plan) if plan.is_executable() => format!(
            "{} link{} to create",
            plan.targets()
                .iter()
                .filter(|target| target.is_work())
                .count(),
            if plan
                .targets()
                .iter()
                .filter(|target| target.is_work())
                .count()
                == 1
            {
                ""
            } else {
                "s"
            }
        ),
        InstallPrompt::Preview(_) => "Nothing left to do".to_owned(),
        InstallPrompt::Report(outcome) => match outcome.status() {
            InstallStatus::Installed if outcome.verification().is_complete() => {
                "Installed and verified".to_owned()
            }
            // Nothing disagreed, and something was not checked. The one word a
            // reader scans first must not claim the second of those.
            InstallStatus::Installed => "Installed · not fully verified".to_owned(),
            InstallStatus::NothingToDo => "Nothing was written".to_owned(),
            InstallStatus::PartiallyApplied => "Partly applied".to_owned(),
            InstallStatus::NotApplied => "Nothing was written".to_owned(),
            InstallStatus::VerificationFailed => "Written, but not verified".to_owned(),
            InstallStatus::InstalledUnrecorded => {
                "Written, but not recorded as Skilled's".to_owned()
            }
        },
        InstallPrompt::Failed(_) => "No plan was made".to_owned(),
    }
}

fn install_prompt_lines(prompt: &InstallPrompt, home: &Path, width: u16) -> Vec<Line<'static>> {
    match prompt {
        InstallPrompt::Failed(message) => vec![Line::from(components::badge(
            Tone::Critical,
            &terminal_safe(message),
        ))],
        InstallPrompt::Preview(plan) => install_plan_lines(plan, home, width),
        InstallPrompt::Report(outcome) => install_report_lines(outcome),
    }
}

fn render_repair_prompt(
    frame: &mut Frame<'_>,
    area: Rect,
    prompt: &RepairPrompt,
    scroll: usize,
    extent: Option<usize>,
    fully_seen: bool,
) {
    let popup = install_prompt_popup(area);
    frame.render_widget(Clear, popup);
    let (title, scope) = match prompt {
        RepairPrompt::Preview(_) | RepairPrompt::Failed(_) => {
            ("Repair skill", "nothing written yet")
        }
        RepairPrompt::Report(_) => ("Repair result", "already applied"),
    };
    let actions = repair_prompt_actions(prompt, fully_seen);
    let regions = install_prompt_regions(area, u16::try_from(actions.width()).unwrap_or(u16::MAX));
    frame.render_widget(components::dialog_frame(title, scope), popup);
    frame.render_widget(
        Paragraph::new(repair_prompt_lines(prompt, regions.body.width))
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
        regions.body,
    );
    frame.render_widget(
        Paragraph::new(components::rule(regions.divider.width)),
        regions.divider,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            repair_prompt_status(prompt, scroll, extent),
            theme::key_label(),
        ))),
        regions.status,
    );
    frame.render_widget(Paragraph::new(actions.right_aligned()), regions.actions);
}

fn repair_prompt_actions(prompt: &RepairPrompt, fully_seen: bool) -> Line<'static> {
    let confirm =
        fully_seen && matches!(prompt, RepairPrompt::Preview(plan) if plan.is_executable());
    let mut spans = Vec::new();
    if confirm {
        spans.extend([
            Span::styled("Enter", theme::key_cap()),
            Span::raw(" "),
            Span::styled("Repair", theme::key_label()),
            Span::raw("   "),
        ]);
    }
    spans.extend([
        Span::styled("Esc", theme::key_cap()),
        Span::raw(" "),
        Span::styled(if confirm { "Cancel" } else { "Close" }, theme::key_label()),
    ]);
    Line::from(spans)
}

fn repair_prompt_status(prompt: &RepairPrompt, scroll: usize, extent: Option<usize>) -> String {
    let verdict = match prompt {
        RepairPrompt::Preview(plan) if plan.blocking_finding().is_some() => {
            "Blocked — nothing will be written".to_owned()
        }
        RepairPrompt::Preview(plan) if plan.is_executable() => "1 link to replace".to_owned(),
        RepairPrompt::Preview(_) => "Nothing to repair".to_owned(),
        RepairPrompt::Report(outcome) => match outcome.status() {
            RepairStatus::NothingToRepair => "Nothing was written".to_owned(),
            RepairStatus::Repaired if outcome.verification().is_complete() => {
                "Repaired and verified".to_owned()
            }
            RepairStatus::Repaired => "Repaired · not fully verified".to_owned(),
            RepairStatus::NotApplied => "Nothing was written".to_owned(),
            RepairStatus::PartiallyApplied => "Partly applied · manual recovery needed".to_owned(),
            RepairStatus::RepairedUnrecorded => {
                "Repaired, but not recorded as Skilled's".to_owned()
            }
            RepairStatus::VerificationFailed => "Repaired, but not verified".to_owned(),
        },
        RepairPrompt::Failed(_) => "No plan was made".to_owned(),
    };
    match extent {
        Some(extent) if extent > 0 => {
            let where_to = match (scroll > 0, scroll < extent) {
                (true, true) => "more above and below",
                (true, false) => "more above",
                _ => "more below",
            };
            format!("{verdict} · {where_to}")
        }
        _ => verdict,
    }
}

fn repair_prompt_lines(prompt: &RepairPrompt, _width: u16) -> Vec<Line<'static>> {
    match prompt {
        RepairPrompt::Failed(message) => vec![Line::from(components::badge(
            Tone::Critical,
            &terminal_safe(message),
        ))],
        RepairPrompt::Preview(plan) => repair_plan_lines(plan),
        RepairPrompt::Report(outcome) => repair_report_lines(outcome),
    }
}

fn repair_plan_lines(plan: &RepairPlan) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::styled(
            format!(
                "Skill: {} · {}",
                terminal_safe(plan.skill_name()),
                plan.agent().display_name()
            ),
            theme::section_title(),
        ),
        Line::from(format!(
            "Link: {}",
            terminal_safe(&plan.link_path().display().to_string())
        )),
    ];
    if !plan.current_target().as_os_str().is_empty() {
        lines.push(Line::from(format!(
            "Old target: {}",
            terminal_safe(&plan.current_target().display().to_string())
        )));
    }
    if let Some(target) = plan.new_target() {
        lines.push(Line::from(format!(
            "New target: {}",
            terminal_safe(&target.display().to_string())
        )));
    }
    if let Some(label) = plan.old_source_label() {
        lines.push(Line::from(format!(
            "Recorded source: {}",
            terminal_safe(label)
        )));
    } else {
        lines.push(Line::from("Recorded source: unavailable in this receipt"));
    }
    if let Some(label) = plan.new_source_label() {
        lines.push(Line::from(format!(
            "Selected source: {}",
            terminal_safe(label)
        )));
    }
    if plan.source_changed() {
        lines.push(Line::from(components::badge(
            Tone::Warning,
            "The registry now selects a different source.",
        )));
    }
    if let Some(outlook) = plan.opencode_outlook() {
        lines.push(Line::from(format!(
            "OpenCode after repair: {}",
            terminal_safe(&outlook.preview_summary())
        )));
    }
    match plan.disposition() {
        RepairDisposition::ReplaceLink { dangling: true } => lines.push(Line::from(
            "Action: atomically replace the dangling symbolic link where supported",
        )),
        RepairDisposition::ReplaceLink { dangling: false } => lines.push(Line::from(
            "Action: atomically replace the incorrect symbolic link where supported",
        )),
        RepairDisposition::NothingToRepair => {
            lines.push(Line::from("Action: nothing; this link is already correct"))
        }
        RepairDisposition::Blocked { finding } => lines.push(Line::from(components::badge(
            Tone::Critical,
            &format!("{} — {}", finding.code(), terminal_safe(finding.evidence())),
        ))),
    }
    for warning in plan.warnings() {
        lines.push(Line::from(components::badge(
            Tone::Warning,
            &terminal_safe(warning),
        )));
    }
    lines
}

fn repair_report_lines(outcome: &RepairOutcome) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled(
        format!("Result: {:?}", outcome.status()),
        theme::section_title(),
    )];
    if let Some(step) = outcome.applied().step() {
        let text = match step.outcome() {
            RepairStepOutcome::Repaired => "link replaced and receipt recorded".to_owned(),
            RepairStepOutcome::RepairedUnrecorded(error) => format!(
                "link replaced, but its receipt failed: {}",
                terminal_safe(error)
            ),
            RepairStepOutcome::RemovedUnreplaced(error) => format!(
                "original link removed without replacement: {}",
                terminal_safe(error)
            ),
            RepairStepOutcome::ResidualTemporary { path, error } => format!(
                "an object was preserved at {}: {}",
                terminal_safe(&path.display().to_string()),
                terminal_safe(error)
            ),
            RepairStepOutcome::RepairedResidualTemporary { path, error } => format!(
                "link replaced and receipt recorded, but an object was left at {}: {}",
                terminal_safe(&path.display().to_string()),
                terminal_safe(error)
            ),
            RepairStepOutcome::MovedRootUnreceipted { path, error } => format!(
                "live replacement written without a receipt at {}: {}",
                terminal_safe(&path.display().to_string()),
                terminal_safe(error)
            ),
            RepairStepOutcome::Failed(reason) => {
                format!("nothing written: {}", terminal_safe(reason))
            }
        };
        lines.push(Line::from(text));
        lines.push(Line::from(terminal_safe(
            &step.link_path().display().to_string(),
        )));
    }
    for withheld in outcome.verification().withheld() {
        lines.push(Line::from(format!(
            "Not established: {} — {}",
            withheld.agent().display_name(),
            terminal_safe(withheld.reason())
        )));
    }
    for failure in outcome.verification().failures() {
        lines.push(Line::from(format!(
            "Not verified: {} — {}",
            failure.agent().display_name(),
            terminal_safe(failure.observed())
        )));
    }
    lines
}

fn install_plan_lines(plan: &InstallPlan, home: &Path, width: u16) -> Vec<Line<'static>> {
    let blocked = plan.is_blocked();
    let mut lines = vec![
        Line::styled(
            format!("Skill: {}", terminal_safe(plan.skill_name())),
            theme::section_title(),
        ),
        Line::from(format!(
            "From: {} · {}",
            terminal_safe(plan.variant().source_label()),
            terminal_safe(&plan.variant().catalog_relative_path().display().to_string())
        )),
        Line::from(format!(
            "Links to: {}",
            terminal_safe(&plan.source_dir().display().to_string())
        )),
        Line::default(),
        Line::styled("Targets", theme::section_title()),
    ];
    for target in plan.targets() {
        lines.extend(install_target_lines(target, blocked, width));
    }
    if !plan.warnings().is_empty() {
        lines.push(Line::default());
        lines.push(Line::styled("Before you confirm", theme::section_title()));
        for warning in plan.warnings() {
            lines.push(Line::from(components::badge(
                Tone::Warning,
                &terminal_safe(warning),
            )));
        }
    }
    // The home directory is the one thing a preview may relate a path to, and
    // only as a note beside the absolute paths above it.
    lines.push(Line::default());
    lines.push(Line::styled(
        format!("Home: {}", terminal_safe(&home.display().to_string())),
        theme::key_label(),
    ));
    lines
}

/// One target: a verdict short enough to stay on one row, the exact path
/// beneath it, and the evidence beneath that where there is any.
///
/// Split three ways because only the first line carries a tone glyph. A verdict
/// that wrapped would show its badge on the first row and continue at the
/// margin on the next, which reads as a new statement rather than as the rest
/// of the one above it.
fn install_target_lines(
    target: &InstallTarget,
    plan_is_blocked: bool,
    width: u16,
) -> Vec<Line<'static>> {
    // A plan blocks whole, so a target that would have been work is not work.
    // Reading the rule under the rule and green ticks above it would be the
    // screen contradicting itself in the channel a reader scans first.
    let work_tone = if plan_is_blocked {
        Tone::Inactive
    } else {
        Tone::Healthy
    };
    let would = if plan_is_blocked { "would " } else { "" };
    let (tone, verdict, detail) = match target.disposition() {
        TargetDisposition::CreateLink => (work_tone, format!("{would}create the link"), None),
        TargetDisposition::CreateRootAndLink => (
            work_tone,
            format!("{would}create the skill root, then the link"),
            None,
        ),
        // Already installed is an observation rather than work, so it reads the
        // same whether or not another target blocked this plan. What it claims
        // is a receipt for the path, which is what Skilled actually holds.
        TargetDisposition::AlreadyInstalled { receipted: true } => (
            Tone::Healthy,
            "already installed, and Skilled holds a receipt for this path".to_owned(),
            None,
        ),
        TargetDisposition::AlreadyInstalled { receipted: false } => (
            Tone::Unmanaged,
            "already in place, and Skilled holds no receipt for it".to_owned(),
            None,
        ),
        TargetDisposition::Excluded { reason } => {
            let (verdict, detail) = excluded_reason(reason);
            (Tone::Unmanaged, verdict, detail)
        }
        TargetDisposition::Blocked { finding } => (
            Tone::Critical,
            finding.code().to_owned(),
            Some(terminal_safe(finding.evidence())),
        ),
    };
    let mut lines = vec![
        Line::from(components::badge(
            tone,
            &format!("{}: {verdict}", target.agent().display_name()),
        )),
        // The path is on its own line, in full: it is the thing being agreed
        // to, and a line that had to compete with a verdict for room would be
        // the one that got shortened.
        Line::from(format!(
            "    {}",
            terminal_safe(&target.link_path().display().to_string())
        )),
    ];
    if let Some(detail) = detail {
        lines.extend(indented_detail(&detail, width));
    }
    if plan_is_blocked && target.is_work() {
        lines.extend(indented_detail(
            "nothing will be written here: this plan is blocked",
            width,
        ));
    }
    lines
}

/// A sentence set under its target, wrapped by hand so every row of it keeps
/// the same indent.
///
/// The paragraph's own wrapping restarts at the dialog margin, which reads as a
/// new statement rather than as the rest of the one above it — the very thing
/// splitting the target across three lines was meant to avoid. Words longer
/// than the room left are placed anyway and allowed to wrap: cutting a word out
/// of an explanation is worse than one ragged row.
fn indented_detail(detail: &str, width: u16) -> Vec<Line<'static>> {
    const INDENT: &str = "    ";
    let room = usize::from(width).saturating_sub(INDENT.len()).max(1);
    let mut lines = Vec::new();
    let mut row = String::new();
    for word in detail.split_whitespace() {
        let candidate = if row.is_empty() {
            word.to_owned()
        } else {
            format!("{row} {word}")
        };
        if Span::raw(&candidate).width() > room && !row.is_empty() {
            lines.push(Line::styled(format!("{INDENT}{row}"), theme::key_label()));
            row = word.to_owned();
        } else {
            row = candidate;
        }
    }
    if !row.is_empty() {
        lines.push(Line::styled(format!("{INDENT}{row}"), theme::key_label()));
    }
    lines
}

fn excluded_reason(reason: &ExcludedReason) -> (String, Option<String>) {
    match reason {
        ExcludedReason::NotConfigured => (
            "not configured, so Skilled leaves it alone".to_owned(),
            None,
        ),
        ExcludedReason::NotRequested => ("not named by this request".to_owned(), None),
        ExcludedReason::Incompatible => (
            "cannot use this variant, so there is nothing to install".to_owned(),
            None,
        ),
        ExcludedReason::AgentSpecificOverride { selected } => (
            "prefers its own edition".to_owned(),
            Some(format!(
                "installing this one would not change what it loads: it resolves {}",
                terminal_safe(&selected.evidence_label())
            )),
        ),
    }
}

/// What one applied step did, and how strongly the report says it.
///
/// A step's reason carries paths and operating-system error text, which is
/// outside Skilled's control and escaped like everything else that comes from
/// there.
fn install_step_verdict(outcome: &StepOutcome) -> (Tone, String) {
    match outcome {
        StepOutcome::Created => (Tone::Healthy, "link created".to_owned()),
        StepOutcome::Removed => (Tone::Healthy, "link removed".to_owned()),
        StepOutcome::CreatedUnrecorded(error) => (
            Tone::Warning,
            format!(
                "link created, but Skilled could not record owning it: {}",
                terminal_safe(&error.to_string())
            ),
        ),
        StepOutcome::RootCreatedLinkFailed(error) => (
            Tone::Critical,
            format!(
                "skill root created, but the link was not: {}",
                terminal_safe(error)
            ),
        ),
        StepOutcome::Failed(reason) => (
            Tone::Critical,
            format!("not written — {}", terminal_safe(reason)),
        ),
        StepOutcome::Unattempted => (
            Tone::Unmanaged,
            "not attempted, because an earlier step stopped the run".to_owned(),
        ),
    }
}

fn uninstall_step_verdict(outcome: &StepOutcome) -> (Tone, String) {
    match outcome {
        StepOutcome::Removed => (Tone::Healthy, "link removed".to_owned()),
        StepOutcome::Failed(reason) => (
            Tone::Critical,
            format!("not removed — {}", terminal_safe(reason)),
        ),
        StepOutcome::Unattempted => (
            Tone::Unmanaged,
            "not attempted, because an earlier step stopped the run".to_owned(),
        ),
        other => install_step_verdict(other),
    }
}

fn install_report_lines(outcome: &InstallOutcome) -> Vec<Line<'static>> {
    let plan = outcome.plan();
    let mut lines = vec![
        Line::styled(
            format!("Skill: {}", terminal_safe(plan.skill_name())),
            theme::section_title(),
        ),
        Line::from(format!(
            "Links to: {}",
            terminal_safe(&plan.source_dir().display().to_string())
        )),
        Line::default(),
        Line::styled("Steps", theme::section_title()),
    ];
    if outcome.applied().steps().is_empty() {
        lines.push(Line::from("Nothing was written."));
    }
    for step in outcome.applied().steps() {
        let (tone, verdict) = install_step_verdict(step.outcome());
        lines.push(Line::from(components::badge(
            tone,
            &format!("{}: {verdict}", step.agent().display_name()),
        )));
        lines.push(Line::from(format!(
            "    {}",
            terminal_safe(&step.link_path().display().to_string())
        )));
    }
    lines.push(Line::default());
    lines.push(Line::styled("Verification", theme::section_title()));
    if outcome.verification().is_complete() {
        lines.push(Line::from(components::badge(
            Tone::Healthy,
            "every link written was observed again and matches this plan",
        )));
    } else if outcome.verification().is_verified() {
        lines.push(Line::from(components::badge(
            Tone::Healthy,
            "every link written was observed again, and nothing disagreed with this plan",
        )));
    }
    for withheld in outcome.verification().withheld() {
        lines.push(Line::from(components::badge(
            Tone::Inactive,
            &format!(
                "{}: {}",
                withheld.agent().display_name(),
                terminal_safe(withheld.reason())
            ),
        )));
    }
    for failure in outcome.verification().failures() {
        lines.push(Line::from(components::badge(
            Tone::Critical,
            &format!(
                "{}: {}",
                failure.agent().display_name(),
                terminal_safe(failure.observed())
            ),
        )));
    }
    // Only where something was written: there is nothing to say about undoing
    // an operation that wrote nothing.
    if outcome.status() != InstallStatus::Installed
        && outcome
            .applied()
            .steps()
            .iter()
            .any(AppliedStep::changed_filesystem)
    {
        lines.push(Line::default());
        lines.push(Line::from(
            "Skilled does not undo a partial install automatically; uninstall is a separate \
             confirmed operation, and repair only replaces a still-present link whose \
             ownership can be proven.",
        ));
    }
    lines
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

fn render_settings(frame: &mut Frame<'_>, area: Rect, app: &SkilledApp) {
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
    let mut lines = vec![
        Line::styled("Setup", theme::section_title()),
        Line::default(),
        components::list_row(vec![Span::raw("Rerun setup")], true, regions.body.width),
        Line::default(),
    ];
    if app.metadata_failure().is_some() {
        lines.extend([
            Line::from("Rerunning setup is unavailable while metadata cannot be written."),
            Line::from("Inventory, Sources, and Doctor remain available read-only."),
            Line::from("Esc closes Settings."),
        ]);
    } else {
        lines.extend([
            Line::from("Reset setup completion and return to Welcome."),
            Line::from("Agent root and executable detection is refreshed."),
            Line::from("Agent selections and registered sources are retained."),
            Line::from("Enter reruns setup; Esc closes Settings."),
        ]);
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
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
            Line::from(if app.can_rerun_setup() {
                vec![
                    Span::styled("Enter", theme::key_cap()),
                    Span::raw(" "),
                    Span::styled("Rerun", theme::key_label()),
                    Span::raw("   "),
                    Span::styled("Esc", theme::key_cap()),
                    Span::raw(" "),
                    Span::styled("Close", theme::key_label()),
                ]
            } else {
                vec![
                    Span::styled("Esc", theme::key_cap()),
                    Span::raw(" "),
                    Span::styled("Close", theme::key_label()),
                ]
            })
            .right_aligned(),
        ),
        regions.actions,
    );
}

fn render_help(
    frame: &mut Frame<'_>,
    area: Rect,
    context: View,
    app: &SkilledApp,
    findings: &[DoctorItem<'_>],
    detail_extent: Option<usize>,
) {
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
    let commands = help_commands(context, app, findings, detail_extent);
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

fn help_commands(
    context: View,
    app: &SkilledApp,
    findings: &[DoctorItem<'_>],
    detail_extent: Option<usize>,
) -> Vec<HelpCommand> {
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
            if can_scroll_detail(app, detail_extent) {
                commands.push(HelpCommand {
                    key: "Up/Down or j/k",
                    label: "Scroll details",
                    description: "reach the rows the region cannot show at once",
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
            if app.can_uninstall_selection() {
                commands.push(HelpCommand {
                    key: "x",
                    label: "Uninstall",
                    description: "remove only matching Skilled-managed links",
                });
            }
            commands.extend([
                HelpCommand {
                    key: "2",
                    label: "Sources",
                    description: "open registered sources",
                },
                HelpCommand {
                    key: "4",
                    label: "Doctor",
                    description: "open the findings list",
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
            if app.can_forget_source() {
                commands.push(HelpCommand {
                    key: "x",
                    label: "Forget",
                    description: "remove inactive source metadata only",
                });
            }
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
            if app.can_install_selection() {
                commands.push(HelpCommand {
                    key: "i",
                    label: "Install",
                    description: "preview installing the focused variant",
                });
            }
            if app.can_add_source() {
                commands.push(HelpCommand {
                    key: "a",
                    label: "Add source",
                    description: "inspect a local checkout",
                });
            }
            commands.extend([
                HelpCommand {
                    key: "1",
                    label: "Inventory",
                    description: "return to Inventory",
                },
                HelpCommand {
                    key: "4",
                    label: "Doctor",
                    description: "open the findings list",
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
        View::Updates => {
            let mut commands = vec![HelpCommand {
                key: "Tab / Shift-Tab",
                label: "Region",
                description: "move region focus",
            }];
            if !app.sources().is_empty() {
                commands.push(HelpCommand {
                    key: "Up/Down or j/k",
                    label: if app.updates_pane() == UpdatesPane::Details {
                        "Scroll"
                    } else {
                        "Move"
                    },
                    description: "move the selected source or scroll details",
                });
                let can_open = !app.update_check_in_flight()
                    && (app.updates_pane() == UpdatesPane::Candidates
                        || app.selected_update_source().is_some_and(|source| {
                            app.update_check_for(source.id()).is_some_and(|check| {
                                !check.superseded_by(source)
                                    && check.verdict == RepositoryUpdateVerdict::Available
                            })
                        }));
                if can_open {
                    commands.push(HelpCommand {
                        key: "Enter",
                        label: "Open",
                        description: "open details, then preview an available fast-forward",
                    });
                }
                if !app.update_check_in_flight() {
                    commands.push(HelpCommand {
                        key: "u",
                        label: "Check",
                        description: "explicitly fetch every registered source",
                    });
                }
            }
            commands.extend([
                HelpCommand {
                    key: "Esc",
                    label: "Back / cancel",
                    description: "leave details, return to Inventory, or cancel an active check",
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
        View::Doctor => {
            let mut commands = vec![HelpCommand {
                key: "Tab / Shift-Tab",
                label: "Region",
                description: "move region focus forward or backward",
            }];
            if doctor_can_move_selection(app) {
                commands.push(HelpCommand {
                    key: "Up/Down or j/k",
                    label: "Move",
                    description: "move the selected finding",
                });
            }
            if can_scroll_detail(app, detail_extent) {
                commands.push(HelpCommand {
                    key: "Up/Down or j/k",
                    label: "Scroll details",
                    description: "reach the rows the region cannot show at once",
                });
            }
            if doctor_can_advance(app) {
                commands.push(HelpCommand {
                    key: "Enter",
                    label: "Open details",
                    description: "show everything observed about the finding",
                });
            }
            if doctor_can_repair_selection(app, findings) {
                commands.push(HelpCommand {
                    key: "r",
                    label: "Repair",
                    description: "preview replacing this proven Skilled-owned link",
                });
            }
            commands.extend([
                HelpCommand {
                    key: "1",
                    label: "Inventory",
                    description: "return to Inventory",
                },
                HelpCommand {
                    key: "2",
                    label: "Sources",
                    description: "open registered sources",
                },
                HelpCommand {
                    key: "Esc",
                    label: "Back",
                    description: "leave the detail region, then Doctor",
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
        View::Settings => {
            let mut commands = Vec::new();
            if app.can_rerun_setup() {
                commands.push(HelpCommand {
                    key: "Enter",
                    label: "Rerun setup",
                    description: "reset setup and start again",
                });
            }
            commands.extend([
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
            ]);
            commands
        }
    }
}

fn help_scope(context: View) -> String {
    match context {
        View::Setup(step) => format!("Setup · {}", step.title()),
        View::Inventory => "Inventory".to_owned(),
        View::Sources => "Sources".to_owned(),
        View::Updates => "Updates".to_owned(),
        View::Doctor => "Doctor".to_owned(),
        View::Settings => "Settings".to_owned(),
    }
}

fn render_footer(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &SkilledApp,
    findings: &[DoctorItem<'_>],
    detail_extent: Option<usize>,
    update_preview_seen: Option<bool>,
) {
    // The band reaches the full width, so the row reads as chrome rather than
    // as a smear the length of the hints. The hint line itself only sets
    // foreground colours, apart from the key caps' own emphasis.
    frame.render_widget(Block::new().style(theme::chrome_band()), area);
    // On a tall terminal the band is the prototype's 36px key bar — two rows
    // — and the hints keep the last: the spare row falls between the
    // workspace and the hints, mirroring the title bar at the other edge.
    let hints = Rect {
        y: area.y + area.height.saturating_sub(1),
        height: 1.min(area.height),
        ..area
    };
    frame.render_widget(
        Paragraph::new(components::key_hint_line(
            &context_key_hints(app, findings, detail_extent, update_preview_seen),
            hints.width,
        ))
        .style(theme::chrome()),
        hints,
    );
}

/// The commands the active context actually handles.
///
/// This mirrors [`crate::input`]. A hint that is not backed by a key mapping is
/// a promise the application cannot keep, so unimplemented commands are absent
/// by construction.
///
/// The row is budgeted, and every context declares its routes before `?` and
/// `q`: where they do not all fit, the route survives and the two commands the
/// overlay `?` opens still lists are the ones shed, with the overflow mark
/// saying so. Sources and a drilled-in Doctor both reach that point at eighty
/// columns.
///
/// Sources goes one step further and declares `i · Install` ahead of its
/// routes, which at eighty columns sheds one of them. That is deliberate: the
/// navigation row above already shows every route beside its own key digit,
/// so a route shed from this row is still on screen, while `i` appears nowhere
/// else and acts on the very row the user is standing on.
fn context_key_hints(
    app: &SkilledApp,
    findings: &[DoctorItem<'_>],
    detail_extent: Option<usize>,
    update_preview_seen: Option<bool>,
) -> Vec<KeyHint> {
    if app.help_context().is_some() {
        return vec![
            KeyHint::essential("Esc", "Close"),
            KeyHint::new("Ctrl-C", "Quit"),
        ];
    }
    // The install dialog answers for the whole row while it is open, and only
    // offers a confirmation where the reducer would accept one.
    if let Some(prompt) = app.pending_operation() {
        let mut hints = Vec::new();
        if detail_extent.is_some_and(|extent| extent > 0) {
            hints.push(KeyHint::essential("j/k", "Scroll"));
        }
        // Enter appears only where the reducer would accept it, which is a
        // plan with work left whose last row has been on screen. Measured from
        // this frame rather than from the offset the application last noted,
        // so the hint cannot survive a resize that put content back under the
        // window; the runner notes the same measurement before reading the key,
        // so the two agree at the moment one is pressed.
        let (executable, label) = match prompt {
            OperationPrompt::Install(InstallPrompt::Preview(plan)) => {
                (plan.is_executable(), "Install")
            }
            OperationPrompt::Uninstall(UninstallPrompt::Preview(plan)) => {
                (plan.is_executable(), "Uninstall")
            }
            OperationPrompt::Forget(ForgetPrompt::Preview(plan)) => {
                (plan.is_executable(), "Forget")
            }
            OperationPrompt::Install(_) => (false, "Install"),
            OperationPrompt::Uninstall(_) => (false, "Uninstall"),
            OperationPrompt::Forget(_) => (false, "Forget"),
        };
        if operation_preview_fully_seen(app, detail_extent) && executable {
            hints.push(KeyHint::essential("Enter", label));
            hints.push(KeyHint::essential("Esc", "Cancel"));
        } else {
            hints.push(KeyHint::essential("Esc", "Close"));
        }
        hints.push(KeyHint::new("Ctrl-C", "Quit"));
        return hints;
    }
    if let Some(prompt) = app.pending_repair() {
        let mut hints = Vec::new();
        if detail_extent.is_some_and(|extent| extent > 0) {
            hints.push(KeyHint::essential("j/k", "Scroll"));
        }
        if preview_fully_seen(app, detail_extent)
            && matches!(prompt, RepairPrompt::Preview(plan) if plan.is_executable())
        {
            hints.push(KeyHint::essential("Enter", "Repair"));
            hints.push(KeyHint::essential("Esc", "Cancel"));
        } else {
            hints.push(KeyHint::essential("Esc", "Close"));
        }
        hints.push(KeyHint::new("Ctrl-C", "Quit"));
        return hints;
    }
    if let Some(prompt) = app.pending_update() {
        let mut hints = Vec::new();
        if detail_extent.is_some_and(|extent| extent > 0) {
            hints.push(KeyHint::essential("j/k", "Scroll"));
        }
        if matches!(prompt, RepositoryUpdatePrompt::Preview(plan) if !plan.is_blocked())
            && (app.update_preview_fully_seen() || update_preview_seen == Some(true))
        {
            hints.push(KeyHint::essential("Enter", "Apply"));
            hints.push(KeyHint::essential("Esc", "Cancel"));
        } else {
            hints.push(KeyHint::essential("Esc", "Close"));
        }
        hints.push(KeyHint::new("Ctrl-C", "Quit"));
        return hints;
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
            if app.can_uninstall_selection() {
                hints.push(KeyHint::new("x", "Uninstall"));
            }
            if inventory_can_move_selection(app) {
                hints.push(KeyHint::new("j/k", "Move"));
            }
            if can_scroll_detail(app, detail_extent) {
                hints.push(KeyHint::new("j/k", "Scroll"));
            }
            if inventory_can_advance(app) {
                hints.push(KeyHint::essential("Enter", "Open"));
            }
            if app.can_filter_inventory() {
                hints.push(KeyHint::new("/", "Filter"));
            }
            hints.extend([
                KeyHint::new("2", "Sources"),
                KeyHint::new("4", "Doctor"),
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
            if app.can_forget_source() {
                hints.push(KeyHint::new("x", "Forget"));
            }
            if sources_can_move_selection(app) {
                hints.push(KeyHint::new("j/k", "Move"));
            }
            if sources_can_advance(app) {
                hints.push(KeyHint::essential("Enter", "Open"));
            }
            if app.can_install_selection() {
                hints.push(KeyHint::new("i", "Install"));
            }
            if app.can_add_source() {
                hints.push(KeyHint::new("a", "Add source"));
            }
            hints.extend([
                KeyHint::new("1", "Inventory"),
                KeyHint::new("4", "Doctor"),
                KeyHint::new("?", "Help"),
                KeyHint::new("q", "Quit"),
                KeyHint::essential("Esc", "Back"),
            ]);
            hints
        }
        View::Updates => {
            let mut hints = vec![KeyHint::new("Tab/Shift-Tab", "Region")];
            if !app.sources().is_empty() {
                hints.push(KeyHint::new(
                    "j/k",
                    if app.updates_pane() == UpdatesPane::Details {
                        "Scroll"
                    } else {
                        "Move"
                    },
                ));
                let can_open = !app.update_check_in_flight()
                    && (app.updates_pane() == UpdatesPane::Candidates
                        || app.selected_update_source().is_some_and(|source| {
                            app.update_check_for(source.id()).is_some_and(|check| {
                                !check.superseded_by(source)
                                    && check.verdict == RepositoryUpdateVerdict::Available
                            })
                        }));
                if can_open {
                    hints.push(KeyHint::essential("Enter", "Open"));
                }
                if app.update_check_in_flight() {
                    hints.push(KeyHint::essential("Esc", "Cancel check"));
                } else {
                    hints.push(KeyHint::essential("u", "Check"));
                }
            }
            hints.extend([
                KeyHint::new("1", "Inventory"),
                KeyHint::new("2", "Sources"),
                KeyHint::new("4", "Doctor"),
                KeyHint::new("?", "Help"),
                KeyHint::new("q", "Quit"),
            ]);
            if !app.update_check_in_flight() {
                hints.push(KeyHint::essential("Esc", "Back"));
            }
            hints
        }
        View::Doctor => {
            let mut hints = vec![KeyHint::new("Tab/Shift-Tab", "Region")];
            if doctor_can_move_selection(app) {
                hints.push(KeyHint::new("j/k", "Move"));
            }
            if can_scroll_detail(app, detail_extent) {
                hints.push(KeyHint::new("j/k", "Scroll"));
            }
            if doctor_can_advance(app) {
                hints.push(KeyHint::essential("Enter", "Open"));
            }
            if doctor_can_repair_selection(app, findings) {
                hints.push(KeyHint::new("r", "Repair"));
            }
            hints.extend([
                KeyHint::new("1", "Inventory"),
                KeyHint::new("2", "Sources"),
                KeyHint::new("?", "Help"),
                KeyHint::new("q", "Quit"),
                KeyHint::essential("Esc", "Back"),
            ]);
            hints
        }
        View::Settings => {
            let mut hints = Vec::new();
            if app.can_rerun_setup() {
                hints.push(KeyHint::essential("Enter", "Rerun setup"));
            }
            hints.extend([
                KeyHint::new("?", "Help"),
                KeyHint::essential("Esc", "Close"),
            ]);
            hints
        }
    }
}

/// Whether the Doctor row selected in this frame offers repair.
///
/// `render` has already paid to merge and order the findings. Reusing that
/// slice keeps the detail, help, and key bar on the same row without another
/// full allocation and sort.
fn doctor_can_repair_selection(app: &SkilledApp, findings: &[DoctorItem<'_>]) -> bool {
    findings
        .get(app.focused_finding())
        .is_some_and(|entry| app.can_repair_finding(entry))
}

/// Whether every row of the open preview has been on screen, as this frame
/// measures it.
fn operation_preview_fully_seen(app: &SkilledApp, detail_extent: Option<usize>) -> bool {
    detail_extent.is_none_or(|extent| app.detail_scroll() >= extent)
}

fn preview_fully_seen(app: &SkilledApp, detail_extent: Option<usize>) -> bool {
    operation_preview_fully_seen(app, detail_extent)
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

/// The detail region's window only moves where the region has the keyboard and
/// the frame just drawn found rows the window does not reach.
///
/// The extent is the frame's own measurement rather than the last one noted on
/// the application, so the hint cannot survive a resize that removed the thing
/// it advertises.
fn can_scroll_detail(app: &SkilledApp, detail_extent: Option<usize>) -> bool {
    let focused = match app.view() {
        View::Inventory => app.inventory_pane() == InventoryPane::Details,
        View::Doctor => app.doctor_pane() == DoctorPane::Details,
        _ => false,
    };
    focused && detail_extent.is_some_and(|extent| extent > 0)
}

/// The findings list only moves where it has the keyboard and more than one
/// place to stand.
fn doctor_can_move_selection(app: &SkilledApp) -> bool {
    app.doctor_pane() == DoctorPane::Findings && app.finding_count() > 1
}

/// Enter only drills in, so it advertises itself only where it can.
///
/// A selection rests on a finding whenever one exists: the reducer clamps the
/// focus to the list on every scan, so the count answers this without the sort
/// that materialising the list would cost on every frame.
fn doctor_can_advance(app: &SkilledApp) -> bool {
    app.doctor_pane() == DoctorPane::Findings && app.finding_count() > 0
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

    #[test]
    fn update_dialog_rows_keep_offsets_beyond_the_paragraph_scroll_limit() {
        let rows = visual_rows(vec![Line::raw("x".repeat(65_540))], 1);

        assert_eq!(rows.len(), 65_540);
        assert_eq!(label_text(&rows[65_536]), "x");
        assert_eq!(label_text(&rows[65_539]), "x");
    }

    #[test]
    fn a_residual_root_is_a_critical_partial_write_in_the_install_report() {
        let (tone, verdict) = install_step_verdict(&StepOutcome::RootCreatedLinkFailed(
            "permission denied".to_owned(),
        ));

        assert_eq!(tone, Tone::Critical);
        assert_eq!(
            verdict,
            "skill root created, but the link was not: permission denied"
        );
    }

    #[test]
    fn a_failed_uninstall_step_says_not_removed() {
        let (tone, verdict) =
            uninstall_step_verdict(&StepOutcome::Failed("the target changed".to_owned()));

        assert_eq!(tone, Tone::Critical);
        assert_eq!(verdict, "not removed — the target changed");
    }

    fn identity(
        user: Option<&str>,
        host: Option<&str>,
        os: Option<&str>,
    ) -> crate::SessionIdentity {
        crate::SessionIdentity {
            user: user.map(str::to_owned),
            host: host.map(str::to_owned),
            os: os.map(str::to_owned),
        }
    }

    /// An absent segment disappears with its separator: the path never shows
    /// a `·` with nothing on one side of it.
    #[test]
    fn a_context_path_omits_absent_segments_without_dangling_separators() {
        let wide = 80;
        assert_eq!(
            context_path(
                &identity(Some("brian"), Some("macbook"), Some("macOS")),
                wide
            ),
            "global · brian@macbook · macOS"
        );
        assert_eq!(
            context_path(&identity(Some("brian"), None, Some("macOS")), wide),
            "global · brian · macOS"
        );
        assert_eq!(
            context_path(&identity(None, Some("macbook"), Some("macOS")), wide),
            "global · macbook · macOS"
        );
        assert_eq!(
            context_path(&identity(Some("brian"), Some("macbook"), None), wide),
            "global · brian@macbook"
        );
        assert_eq!(context_path(&identity(None, None, None), wide), "global");
    }

    /// A path too wide for its row sheds segments whole: host first, then
    /// user, then the operating system, and the scope word never.
    #[test]
    fn a_tight_context_path_sheds_host_then_user_then_operating_system() {
        let full = identity(Some("brian"), Some("macbook"), Some("macOS"));
        let width_of = |path: &str| Span::raw(path).width();

        assert_eq!(
            context_path(&full, width_of("global · brian@macbook · macOS")),
            "global · brian@macbook · macOS"
        );
        // One column short of the full path: the host is shed first.
        assert_eq!(
            context_path(&full, width_of("global · brian@macbook · macOS") - 1),
            "global · brian · macOS"
        );
        assert_eq!(
            context_path(&full, width_of("global · brian · macOS") - 1),
            "global · macOS"
        );
        assert_eq!(
            context_path(&full, width_of("global · macOS") - 1),
            "global"
        );
        // Even a row too narrow for the scope word still names the scope; the
        // layout clips it rather than the path lying about it.
        assert_eq!(context_path(&full, 0), "global");
    }

    /// A control sequence in the user or host is shown escaped, never given
    /// to the terminal to execute.
    #[test]
    fn a_context_path_escapes_identity_text_from_outside_skilled() {
        assert_eq!(
            context_path(
                &identity(Some("bri\u{1b}an"), Some("mac\u{7}book"), None),
                80
            ),
            "global · bri\\u{1b}an@mac\\u{7}book"
        );
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
            u16::try_from(ROW_MARKER_WIDTH + Span::raw(heading).width() + SUBTITLE_GAP + budget)
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
    /// carries no sense of the value it stands for. Every combination is
    /// checked rather than the one a fixture happens to hold, because the
    /// longest two agents can make of it outruns the narrowest region's line
    /// by a single column — which is the whole reason the field is given the
    /// room to wrap instead of a budget to elide against.
    #[test]
    fn every_registration_claim_stands_whole_in_the_narrowest_detail_region() {
        for claude_code in [false, true] {
            for codex in [false, true] {
                for opencode in [false, true] {
                    let claim =
                        registration_claim(Compatibility::from_flags(claude_code, codex, opencode));
                    let stated = label_text(&detail_field_bounded(
                        REGISTRATION_LABEL,
                        &claim,
                        NARROWEST_INNER_WIDTH,
                        REGISTRATION_CLAIM_LINES,
                    ));
                    assert_eq!(
                        stated,
                        format!("{REGISTRATION_LABEL}: {claim}"),
                        "the claim should be stated whole"
                    );
                }
            }
        }
    }

    /// The window's three numbers are one measurement of one region, so they
    /// are checked against every shape a handful of lines can take rather than
    /// against the fixture a screen happens to hold: whatever the geometry,
    /// the rows scrolled past plus the rows shown plus the rows still below
    /// are the rows the region holds, and the offset never runs past them.
    ///
    /// The degenerate heights are the point of the sweep. They are unreachable
    /// through the application — the shell refuses to draw below eighty by
    /// twenty-four — so nothing else in the suite stands between a subtraction
    /// here and a panic in a user's terminal.
    #[test]
    fn every_detail_window_accounts_for_the_rows_it_was_given() {
        for lines in [
            vec![],
            vec![1],
            vec![1, 1, 1],
            vec![3],
            vec![1, 3, 1],
            vec![2, 2, 2, 2],
            vec![5, 1],
            vec![1, 1, 5],
            vec![1, 1, 1, 1, 1, 1, 1, 1],
        ] {
            let total: usize = lines.iter().sum();
            for height in 0..10u16 {
                let extent = detail_max_scroll(&lines, height);
                assert!(
                    extent < lines.len().max(1),
                    "a window cannot open past the last line"
                );
                let mut end = 0;
                // Offsets past the extent are asked for on purpose: the reducer
                // clamps against the previous frame, so a terminal that grew
                // hands this one an offset it has never measured.
                for offset in 0..lines.len() + 3 {
                    let window = detail_window(&lines, height, offset);
                    assert_eq!(
                        window.above + window.shown + window.below,
                        total,
                        "lines {lines:?} at height {height} and offset {offset} \
                         lost or invented rows: {window:?}"
                    );
                    assert_eq!(
                        window.above,
                        lines.iter().take(offset.min(extent)).sum::<usize>(),
                        "lines {lines:?} at height {height} and offset {offset} \
                         opened somewhere other than on a line"
                    );
                    // A region with no rows at all draws neither content nor
                    // notice; every other one has room for what it claims.
                    assert!(
                        height == 0
                            || window.shown
                                + usize::from(window.above > 0)
                                + usize::from(window.below > 0)
                                <= usize::from(height),
                        "lines {lines:?} at height {height} and offset {offset} \
                         needed more rows than the region has: {window:?}"
                    );
                    // Scrolling never takes content back: the foot of the
                    // window only ever moves down the content.
                    assert!(
                        window.above + window.shown >= end,
                        "lines {lines:?} at height {height} and offset {offset} \
                         gave back rows it had already shown: {window:?}"
                    );
                    end = window.above + window.shown;
                }
            }
            // Scrolled to the extent, the last row is on screen: an extent
            // that stops short of the end is not an extent. The exception is
            // a final line taller than the window, which can be opened on but
            // never finished.
            for height in 2..10u16 {
                let window = detail_window(&lines, height, detail_max_scroll(&lines, height));
                assert!(
                    window.below == 0
                        || lines
                            .last()
                            .is_some_and(|last| *last > usize::from(height) - 1),
                    "lines {lines:?} at height {height} could not reach the end: {window:?}"
                );
            }
        }
    }

    /// A field wrapped onto a second row is withheld rather than shown headless
    /// or headed by nothing — except where the line is taller than the window
    /// itself, which has no boundary to stop on and would otherwise leave the
    /// region blank.
    #[test]
    fn the_window_stops_on_a_line_boundary_unless_one_line_outgrows_it() {
        // Five rows: one for the notice below, four for content, and the
        // three-row line fits inside them.
        assert_eq!(detail_window(&[1, 3, 1, 1], 5, 0).shown, 4);
        // One row short, so the three-row line is withheld whole rather than
        // shown as its first two rows.
        assert_eq!(detail_window(&[1, 3, 1, 1], 4, 0).shown, 1);
        // Nothing fits whole, so the region shows what it can rather than
        // nothing at all.
        assert_eq!(detail_window(&[5, 1], 4, 0).shown, 3);
    }

    /// The window opens on a line, so every keystroke moves it: the reader is
    /// never asked to press a key twice for one step. The rows it gives up at
    /// the top are the whole of the line it left behind.
    #[test]
    fn every_step_of_the_window_leaves_a_whole_line_behind() {
        let lines = [1, 3, 1, 1, 2, 1, 3, 1];
        for height in 2..10u16 {
            let extent = detail_max_scroll(&lines, height);
            let mut above = 0;
            for offset in 1..=extent {
                let window = detail_window(&lines, height, offset);
                assert!(
                    window.above > above,
                    "at height {height} the window did not move for offset {offset}"
                );
                assert_eq!(window.above, above + lines[offset - 1]);
                above = window.above;
            }
        }
    }

    /// The dropped-rows count is a measurement, and a measurement that is
    /// wrong is worse than none at all — it is read as a fact about the
    /// terminal rather than as an apology. So it is checked against every
    /// shape three sections can take rather than against the one a fixture
    /// happens to produce: the count must equal the rows the sections wanted
    /// and did not get, where what a section got is what it could actually
    /// fill and never the blank a generous allotment leaves under it.
    #[test]
    fn the_detail_region_reports_exactly_the_rows_its_sections_could_not_show() {
        for repository in 0..5 {
            for catalog in 0..5 {
                for variant in 0..5 {
                    // Deliberately past what each section holds. The render
                    // path derives both essentials from the section's own
                    // lines and so cannot exceed them, but a count that only
                    // holds while its caller behaves is not a measurement, and
                    // the excluded half of this domain is where it would fail.
                    for catalog_essential in 0..6 {
                        for variant_essential in 0..6 {
                            for available in 0..12 {
                                let rows = [repository, catalog, variant];
                                let layout = detail_region_layout(
                                    rows,
                                    [catalog_essential, variant_essential],
                                    available,
                                );
                                let shown: usize = rows
                                    .iter()
                                    .zip(layout.heights)
                                    .map(|(wanted, height)| (*wanted).min(height))
                                    .sum();
                                let total: usize = rows.iter().sum();
                                assert_eq!(
                                    layout.hidden,
                                    total - shown,
                                    "rows {rows:?}, essentials \
                                     [{catalog_essential}, {variant_essential}], \
                                     available {available}, heights {:?}",
                                    layout.heights
                                );
                                assert!(
                                    layout.heights.iter().sum::<usize>() <= available,
                                    "the sections should not outrun the region"
                                );
                                assert_eq!(
                                    layout.hidden == 0,
                                    total <= available,
                                    "a region with room for everything hides nothing, \
                                     and one without says so"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
